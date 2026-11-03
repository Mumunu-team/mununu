//! Integration test for full per-value state-splitting of combinational
//! outputs in the KMTS (`sv-yosys`) pipeline.
//!
//! Consumer: the `joint_mutex_demo_{fixed,bug}` fixtures (a 2-select
//! address decoder + FSM, CWE-1260 address-overlap class — design-pattern
//! demonstrations, NOT real-system findings). The `no_double_sel` property
//! `nu X. (!(sel_a_T_state_V && sel_b_T_state_V) && [] X)` is a JOINT
//! property over two input-dependent combinational signals.
//!
//! Why this needs state-splitting: the earlier ∃-priority labeling tracked
//! each select's *can-be-high* independently, so on the DISJOINT (fixed)
//! variant it reported a SPURIOUS violation (sel_a can be high for one
//! addr, sel_b for a different addr). State-splitting materialises each
//! register-state's JOINTLY-achievable (sel_a, sel_b) assignments as
//! distinct states, so the joint property resolves correctly:
//!   - fixed (disjoint regions)   → no_double_sel HOLDS (true)
//!   - bug   (overlapping region) → no_double_sel FAILS (false)
//!
//! Requires `yosys` on PATH; skips (does not fail) when absent, like the
//! other yosys-dependent integration tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mununu_core::adapter::AdapterOptions;
use mununu_core::{adapter::yosys, context_dsl};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/systemverilog")
}

/// Evaluate every realized formula on the first automaton of the KMTS
/// CTXDSL and reduce each to a single Boolean (satisfied at all initial
/// states), mirroring the verify orchestrator's reduction.
fn kmts_verdicts(fixture: &str) -> Option<BTreeMap<String, bool>> {
    let dir = examples_dir();
    let sv_path = dir.join(format!("{fixture}.sv"));
    let content = std::fs::read_to_string(&sv_path).ok()?;
    let kmts_opts = AdapterOptions {
        sidecar_json: mununu_core::adapter::systemverilog::annotation::find_sidecar(&sv_path)
            .and_then(|p| std::fs::read_to_string(p).ok()),
        ..Default::default()
    };
    let yopts = yosys::YosysOptions {
        primary_source_path: Some(sv_path.display().to_string()),
        per_module_btor: true,
        ..Default::default()
    };
    let outputs = yosys::translate_sv_per_module(&content, &kmts_opts, &yopts).ok()?;
    let ctxdsl = &outputs.first()?.output.ctxdsl;
    let doc = context_dsl::parse(ctxdsl).ok()?;
    let realized = context_dsl::realize_context(&doc, &[]).ok()?;
    let over = realized.context.clts_names().first()?.clone();
    let clts = realized.context.clts(&over)?;
    let env = realized.environment_for(&over);
    let inits: Vec<_> = clts.initial_states().iter().copied().collect();
    if inits.is_empty() {
        return None;
    }
    let mut out = BTreeMap::new();
    for (fname, rf) in realized.formulas.iter() {
        if let Ok(result) = realized.context.evaluate_mu(&over, &rf.formula, &env, None) {
            let satisfied = inits
                .iter()
                .all(|sid| result.get(sid.index()).map(|b| *b).unwrap_or(false));
            out.insert(fname.clone(), satisfied);
        }
    }
    Some(out)
}

#[test]
fn state_splitting_resolves_joint_combinational_mutex() {
    // Skip cleanly if yosys is unavailable.
    let probe = yosys::translate_sv(
        "module probe(input logic c); endmodule",
        &AdapterOptions::default(),
        &yosys::YosysOptions::default(),
    );
    if let Err(e) = &probe {
        let msg = e.to_string().to_lowercase();
        if msg.contains("yosys") && (msg.contains("not found") || msg.contains("locate")) {
            eprintln!("SKIP sv_state_splitting: yosys not available ({e})");
            return;
        }
    }

    let fixed = kmts_verdicts("joint_mutex_demo_fixed")
        .expect("joint_mutex_demo_fixed should lift via the KMTS pipeline");
    let bug = kmts_verdicts("joint_mutex_demo_bug")
        .expect("joint_mutex_demo_bug should lift via the KMTS pipeline");

    // FIXED (disjoint regions): the joint mutex HOLDS. This is the
    // load-bearing assertion — under the old ∃-priority labeling this was
    // a spurious `false`; state-splitting over the joint (sel_a, sel_b)
    // assignment makes it correctly `true`.
    assert_eq!(
        fixed.get("no_double_sel"),
        Some(&true),
        "fixed/no_double_sel must HOLD (disjoint regions); state-splitting \
         should resolve the joint mutex correctly. Verdicts: {fixed:?}"
    );

    // BUG (overlapping region addr∈[8,10)): the joint mutex FAILS — the
    // contrast proving state-splitting is not trivially always-true.
    assert_eq!(
        bug.get("no_double_sel"),
        Some(&false),
        "bug/no_double_sel must FAIL (regions overlap → both selects high \
         for some addr). Verdicts: {bug:?}"
    );

    // Both designs are well-formed (totality holds).
    assert_eq!(fixed.get("safety"), Some(&true), "fixed safety: {fixed:?}");
    assert_eq!(bug.get("safety"), Some(&true), "bug safety: {bug:?}");
}
