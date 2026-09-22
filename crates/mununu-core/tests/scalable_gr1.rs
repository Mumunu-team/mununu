//! Scalable GR(1) integration test — large state counts via Rust API.
//!
//! Builds hub-and-spoke CLTS programmatically for k = 100_000 .. 1_000_000
//! noise states (n=2 pairs; total states = 2n + k + 1).
//! Noise states use a star topology (Hub ↔ each Noise_j), giving O(k) eval time.
//! Evaluates the formula:
//!   (¬GF(Req0) ∨ GF(Grant0)) ∧ (¬GF(Req1) ∨ GF(Grant1))
//!
//! All tests are marked #[ignore] so they do not run in normal `cargo test`.
//! Run explicitly with:
//!   cargo test scalable_gr1 -- --ignored --nocapture
//!
//! Produces timing output for the state-scaling table.

use std::time::Instant;

use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx};
use mununu_core::mu_calculus::{Environment, EvaluationOptions, evaluate_with_options, parser};

// ── CLTS builder ─────────────────────────────────────────────────────────────

/// Build the hub-and-spoke CLTS with `n_pairs` request/grant pairs and
/// `n_noise` noise states in a star topology (Hub ↔ each Noise_j directly).
///
/// States:
///   0         = Hub (initial)
///   1..2n     = Req_i, Grant_i  for i in 0..n_pairs
///   2n+1..end = Noise_j         for j in 0..n_noise
///
/// Total states: 2*n_pairs + n_noise + 1
fn build_clts(n_pairs: usize, n_noise: usize) -> Clts<DefaultStateIdx, DefaultLabelIdx> {
    let mut builder = Clts::builder();

    // Intern labels
    let mut req_labels = Vec::with_capacity(n_pairs);
    let mut grant_labels = Vec::with_capacity(n_pairs);
    let mut done_labels = Vec::with_capacity(n_pairs);
    for i in 0..n_pairs {
        req_labels.push(
            builder
                .labels()
                .intern([format!("req_{i}")])
                .expect("req label"),
        );
        grant_labels.push(
            builder
                .labels()
                .intern([format!("grant_{i}")])
                .expect("grant label"),
        );
        done_labels.push(
            builder
                .labels()
                .intern([format!("done_{i}")])
                .expect("done label"),
        );
    }
    let tick_label = if n_noise > 0 {
        Some(builder.labels().intern(["tick"]).expect("tick label"))
    } else {
        None
    };

    // Create states
    builder.state("Hub");
    let hub_id = builder.state_id_or_insert("Hub").expect("Hub state");
    builder.initial_state_id(hub_id);

    for i in 0..n_pairs {
        builder.state(format!("Req{i}"));
        builder.state(format!("Grant{i}"));
    }

    // Noise states: pre-allocate
    for j in 0..n_noise {
        builder.state(format!("N{j}"));
    }

    // Pair transitions: Hub -> Req_i -> Grant_i -> Hub
    for i in 0..n_pairs {
        let req_id = builder
            .state_id_or_insert(format!("Req{i}"))
            .expect("Req state");
        let grant_id = builder
            .state_id_or_insert(format!("Grant{i}"))
            .expect("Grant state");

        builder.transition_ids(hub_id, &[req_labels[i]], req_id);
        builder.transition_ids(req_id, &[grant_labels[i]], grant_id);
        builder.transition_ids(grant_id, &[done_labels[i]], hub_id);
    }

    // Star topology: Hub <-> N_j directly for each j (nondeterministic on tick).
    // Gives O(1) fixpoint depth regardless of n_noise, so eval scales O(n_noise).
    if n_noise > 0 {
        let tick = tick_label.unwrap();
        for j in 0..n_noise {
            let noise_j = builder
                .state_id_or_insert(format!("N{j}"))
                .expect("noise state");
            builder.transition_ids(hub_id, &[tick], noise_j);
            builder.transition_ids(noise_j, &[tick], hub_id);
        }
    }

    builder.build().expect("CLTS builds successfully")
}

/// Build the Environment with predicate bitsets for each Req_i and Grant_i.
fn build_env(clts: &Clts<DefaultStateIdx, DefaultLabelIdx>, n_pairs: usize) -> Environment {
    use bitvec::prelude::*;

    let count = clts.state_count();
    let mut env = Environment::new(count);

    // State indices:
    //   0         = Hub
    //   2i+1      = Req_i
    //   2i+2      = Grant_i
    //   2n+1..end = Noise states
    for i in 0..n_pairs {
        let req_idx = 2 * i + 1;
        let grant_idx = 2 * i + 2;

        let mut req_set = BitVec::<usize, Lsb0>::repeat(false, count);
        let mut grant_set = BitVec::<usize, Lsb0>::repeat(false, count);

        if req_idx < count {
            req_set.set(req_idx, true);
        }
        if grant_idx < count {
            grant_set.set(grant_idx, true);
        }

        env = env
            .with_predicate(format!("Req{i}"), req_set)
            .with_predicate(format!("Grant{i}"), grant_set);
    }

    env
}

/// Build the GR(1) formula for n pairs using string representation.
///
/// Formula: ∧_{i=0}^{n-1} (¬GF(Req_i) ∨ GF(Grant_i))
/// = ∧_{i=0}^{n-1} ((! (nu NuAi. ((mu MuAi. (Req_i || <> MuAi)) && ([] NuAi))))
///                  || (nu NuGi. ((mu MuGi. (Grant_i || <> MuGi)) && ([] NuGi))))
fn build_formula_string(n_pairs: usize) -> String {
    let pair_exprs: Vec<String> = (0..n_pairs)
        .map(|i| {
            format!(
                "((! (nu NuA{i}. ((mu MuA{i}. (Req{i} || <> MuA{i})) && ([] NuA{i}))))\
                 || (nu NuG{i}. ((mu MuG{i}. (Grant{i} || <> MuG{i})) && ([] NuG{i}))))"
            )
        })
        .collect();
    pair_exprs.join(" && ")
}

// ── Benchmark runner ──────────────────────────────────────────────────────────

fn run_scale(n_pairs: usize, n_noise: usize) {
    let total = 2 * n_pairs + n_noise + 1;
    let formula_str = build_formula_string(n_pairs);
    let formula = parser::parse(&formula_str).expect("formula parses");
    let options = EvaluationOptions::default();

    // Build CLTS
    let t_build = Instant::now();
    let clts = build_clts(n_pairs, n_noise);
    let build_ms = t_build.elapsed().as_millis();
    assert_eq!(clts.state_count(), total, "state count mismatch");

    // Build environment
    let env = build_env(&clts, n_pairs);

    // Evaluate
    let t_eval = Instant::now();
    let result =
        evaluate_with_options(&formula, &clts, &env, &options).expect("evaluation succeeds");
    let eval_ms = t_eval.elapsed().as_millis();

    // All states should satisfy (property is realizable from all states)
    let sat_count = result.count_ones();
    assert_eq!(
        sat_count, total,
        "expected all {total} states to satisfy formula, got {sat_count}"
    );

    println!(
        "n_pairs={n_pairs:2}  n_noise={n_noise:>9}  states={total:>9}  \
         build={build_ms:>6}ms  eval={eval_ms:>6}ms  sat={sat_count}/{total}"
    );
}

// ── Test cases ────────────────────────────────────────────────────────────────

/// Smoke test: n=2, small state counts (runs without --ignored).
#[test]
fn scalable_gr1_smoke() {
    run_scale(2, 0); // 5 states
    run_scale(2, 95); // 100 states
}

/// Track C benchmarks — large state counts (require --ignored).
///
/// Run with:
///   cargo test scalable_gr1_large -- --ignored --nocapture
#[test]
#[ignore]
fn scalable_gr1_large() {
    println!("\n{:=<70}", "");
    println!("Scalable GR(1) — Track C (large states, n=2 pairs)");
    println!("{:=<70}", "");
    println!(
        "{:<10} {:>12} {:>12} {:>9} {:>9} {:>12}",
        "n_pairs", "n_noise", "states", "build(ms)", "eval(ms)", "sat"
    );
    println!("{:-<70}", "");

    // States: 2n + k + 1 where n=2, so states = k + 5
    for &n_noise in &[99_995usize, 249_995, 499_995, 999_995] {
        run_scale(2, n_noise);
    }

    println!("{:=<70}", "");
    println!("Copy eval times into the state-scaling table");
}
