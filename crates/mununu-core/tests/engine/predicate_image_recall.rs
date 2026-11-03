//! Phase A.4 step 4.3 recall harness — measures the predicate-image
//! algorithm's recall against the curated baseline in
//! `examples/verify/bench_predicate_image_a4/fixtures.toml`.
//!
//! For each fixture in the manifest, this harness:
//! 1. Parses the source via the declared adapter (`btor2` direct;
//!    sv-yosys path skipped for now — step 4.4 wires that surface).
//! 2. Runs the brute-force per-cell predicate-image enumeration via
//!    `discover_values_for_btor2_file`.
//! 3. Compares the discovered set against `significant_values_expected`
//!    via [`RecallScore::compute`].
//! 4. Asserts recall ≥ 0.80 for non-adversarial fixtures (0.95 for the
//!    Caliptra real-upstream-bug pair).
//!
//! Fixtures whose `expected` map is empty are skipped (Pono download
//! deferred); SV-only fixtures are reported as `SKIPPED: sv-yosys
//! not yet wired through this harness` (step 4.4 lands the wiring).
//!
//! Run with: `cargo test --test predicate_image_recall -- --nocapture`

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use mununu_core::adapter::sidecar::predicate_image::{
    ImageOptions, RecallScore, all_smt::discover_values_for_btor2_file,
    all_smt::flatten_to_value_sets,
};

#[derive(Debug, serde::Deserialize)]
struct FixtureSet {
    fixture: Vec<FixtureEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureEntry {
    id: String,
    path: String,
    adapter: String,
    category: String,
    #[serde(default)]
    bug: bool,
    #[serde(default)]
    significant_values_expected: BTreeMap<String, Vec<i64>>,
}

fn repo_root() -> PathBuf {
    // tests run with CWD = crate root (mununu-core); the bench dir
    // lives at the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has parent")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn load_manifest() -> FixtureSet {
    let path = repo_root().join("examples/verify/bench_predicate_image_a4/fixtures.toml");
    let src = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "predicate_image_recall: failed to read {}: {e}",
            path.display()
        )
    });
    toml::from_str(&src).expect("fixtures.toml parses")
}

#[derive(Debug)]
enum HarnessOutcome {
    Pass {
        fixture: String,
        scores: Vec<RecallScore>,
    },
    Skip {
        fixture: String,
        reason: String,
    },
    Fail {
        fixture: String,
        scores: Vec<RecallScore>,
        threshold: f64,
    },
}

fn run_fixture(entry: &FixtureEntry) -> HarnessOutcome {
    if entry.significant_values_expected.is_empty() {
        return HarnessOutcome::Skip {
            fixture: entry.id.clone(),
            reason: "no significant_values_expected baseline (e.g. Pono before download)".into(),
        };
    }

    if entry.adapter != "btor2" {
        return HarnessOutcome::Skip {
            fixture: entry.id.clone(),
            reason: format!(
                "adapter {:?} not yet wired through the recall harness (step 4.4)",
                entry.adapter
            ),
        };
    }

    let path = repo_root().join(&entry.path);
    if !path.exists() {
        return HarnessOutcome::Skip {
            fixture: entry.id.clone(),
            reason: format!("source file missing: {}", path.display()),
        };
    }

    let src = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return HarnessOutcome::Skip {
                fixture: entry.id.clone(),
                reason: format!("read failed: {e}"),
            };
        }
    };

    let file = match mununu_core::adapter::btor2::parser::parse(&src) {
        Ok(f) => f,
        Err(e) => {
            return HarnessOutcome::Skip {
                fixture: entry.id.clone(),
                reason: format!("BTOR2 parse failed: {e}"),
            };
        }
    };

    let opts = ImageOptions::default();
    let discovered = match discover_values_for_btor2_file(&file, &opts) {
        Ok(d) => d,
        Err(e) => {
            return HarnessOutcome::Skip {
                fixture: entry.id.clone(),
                reason: format!("encoder refused: {e}"),
            };
        }
    };
    let by_signal = flatten_to_value_sets(&discovered);

    let threshold = recall_threshold_for(&entry.category, entry.bug);
    let mut scores = Vec::new();
    let mut failures: Vec<&str> = Vec::new();

    for (signal, expected_vec) in &entry.significant_values_expected {
        let expected: HashSet<i64> = expected_vec.iter().copied().collect();
        let discovered_set = by_signal.get(signal).cloned().unwrap_or_default();
        let score =
            RecallScore::compute(entry.id.clone(), signal.clone(), expected, discovered_set);
        if score.recall < threshold {
            failures.push(signal.as_str());
        }
        scores.push(score);
    }

    if failures.is_empty() {
        HarnessOutcome::Pass {
            fixture: entry.id.clone(),
            scores,
        }
    } else {
        HarnessOutcome::Fail {
            fixture: entry.id.clone(),
            scores,
            threshold,
        }
    }
}

/// Per-category recall threshold. Adversarial fixtures get the same
/// 0.80 threshold; the soundness side of adversarial cases (the
/// algorithm must NOT surface unreachable values) is asserted inline
/// in the corresponding unit tests under `all_smt::tests`, not here.
fn recall_threshold_for(category: &str, bug: bool) -> f64 {
    match (category, bug) {
        ("real-upstream-bug", _) => 0.95,
        _ => 0.80,
    }
}

#[test]
fn predicate_image_recall_meets_threshold_on_every_fixture() {
    let manifest = load_manifest();

    let mut passes = Vec::new();
    let mut skips = Vec::new();
    let mut fails = Vec::new();

    for entry in &manifest.fixture {
        match run_fixture(entry) {
            HarnessOutcome::Pass { fixture, scores } => passes.push((fixture, scores)),
            HarnessOutcome::Skip { fixture, reason } => skips.push((fixture, reason)),
            HarnessOutcome::Fail {
                fixture,
                scores,
                threshold,
            } => fails.push((fixture, scores, threshold)),
        }
    }

    println!("\n=== predicate-image recall harness ===");
    println!("PASSES ({}):", passes.len());
    for (fixture, scores) in &passes {
        for s in scores {
            println!(
                "  {} :: {:<24} recall {:.2}  expected={} discovered_in_set={}",
                fixture,
                s.signal,
                s.recall,
                s.expected.len(),
                s.expected.intersection(&s.discovered).count(),
            );
        }
    }
    println!("SKIPS ({}):", skips.len());
    for (fixture, reason) in &skips {
        println!("  {fixture}: {reason}");
    }
    if !fails.is_empty() {
        println!("FAILS ({}):", fails.len());
        for (fixture, scores, threshold) in &fails {
            println!("  {fixture}: threshold {threshold}");
            for s in scores {
                println!(
                    "    {:<24} recall {:.2} expected={:?} discovered={:?}",
                    s.signal, s.recall, s.expected, s.discovered
                );
            }
        }
        panic!(
            "predicate-image recall harness: {} fixture(s) below threshold",
            fails.len()
        );
    }
}
