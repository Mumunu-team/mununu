//! R.3 — `KleeneDomain` ↔ `BoolDomain` verdict-baseline regression.
//!
//! Per the KMTS roadmap
//! (`.claude/plans/you-are-a-formal-vast-lake.md` §10.1 R.3 done-criterion):
//!
//! > "Soundness regression suite: every fixture's true / false / ⊥ verdict
//! > is captured in `crates/mununu-core/tests/data/kmts_verdicts.json`;
//! > verdicts re-confirmed on every release."
//!
//! The R.3 ship-bar is the **regression sweep** + the **invariant that
//! `KleeneDomain` produces verdicts that project to the same Booleans
//! `BoolDomain` would emit on a Sharp-everywhere CLTS** (the only data
//! shape every existing adapter produces post-R.1, before CEGAR / UF
//! land in R.5 / R.5b and introduce `KleeneBot` to non-OOB states).
//!
//! This sweep exercises a small, hand-authored CTXDSL fixture set
//! covering propositional, safety (`nu`), reachability (`mu`), and
//! liveness (`nu` + `mu`) formulas. Every fixture is Sharp-everywhere
//! (no `MayOnly`, no `KleeneBot` state-AP), so the projection invariant
//! must hold cell-by-cell — the test fails loudly if it doesn't.
//!
//! The verdict baseline JSON is a stable snapshot. Update via:
//!
//! ```bash
//! MUNUNU_R3_UPDATE_BASELINE=1 cargo test \
//!   -p mununu-core --test r3_kleene_baseline -- --nocapture
//! ```
//!
//! Run with: `cargo test -p mununu-core --test r3_kleene_baseline -- --nocapture`

use std::fs;
use std::path::PathBuf;

use mununu_core::context_dsl;
use mununu_core::mu_calculus::Trit;
use serde_json::json;

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/kmts_verdicts.json")
}

/// Hand-authored CTXDSL fixture covering the four `PropertyClass` shapes
/// (`Propositional`, `Safety`, `Reachability`, `Liveness`) over a
/// Sharp-only 3-state CLTS. The CLTS shape:
///
/// ```text
///   s0 --tick--> s1 --tick--> s2 --reset--> s0
///   s0: {init}
///   s1: {mid}
///   s2: {goal}
/// ```
const FIXTURE_LINEAR_3: &str = r#"
context linear_3 {
    automata {
        automaton tracker {
            controllable {}
            states {
                state s0 initial;
                state s1;
                state s2;
            }
            transitions {
                transition s0 -> s1 on label tick;
                transition s1 -> s2 on label tick;
                transition s2 -> s0 on label reset;
            }
        }
    }
    mu_formulas {
        formula propositional {
            over tracker;
            body = s0;
        }
        formula safety_no_goal_unless {
            over tracker;
            body = nu X. (((! s2) && ([(labels = {tick})] X)) || s2);
        }
        formula reach_goal {
            over tracker;
            body = mu X. (s2 || (<(labels = {tick})> X));
        }
        formula liveness_inf_often {
            over tracker;
            body = nu X. ((mu Y. (s2 || (<(labels = {tick})> Y))) && ([] X));
        }
    }
}
"#;

/// A second hand-authored fixture: a single self-looping state with a
/// "stuck" predicate. Exercises the trivial case where the CLTS has a
/// single state, the modal evaluator's empty-transition-set branches,
/// and the propositional ↔ fixpoint round-trip.
const FIXTURE_SINGLETON: &str = r#"
context singleton {
    automata {
        automaton stuck {
            controllable {}
            states {
                state s0 initial;
            }
            transitions {
                transition s0 -> s0 on label nop;
            }
        }
    }
    mu_formulas {
        formula p_holds {
            over stuck;
            body = s0;
        }
        formula safety_loop_p {
            over stuck;
            body = nu X. (s0 && ([] X));
        }
        formula reach_p {
            over stuck;
            body = mu X. (s0 || (<> X));
        }
    }
}
"#;

fn run_fixture(name: &str, source: &str) -> serde_json::Value {
    let doc = context_dsl::parse(source).expect("CTXDSL parses");
    let realized = context_dsl::realize_context(&doc, &[]).expect("CTXDSL realizes");

    // The fixture's first (and only) automaton.
    let automata_names: Vec<String> = realized.context.clts_names();
    assert_eq!(
        automata_names.len(),
        1,
        "fixture {name}: expected exactly one automaton, got {automata_names:?}"
    );
    let automaton = &automata_names[0];

    let mut per_formula = Vec::new();
    let formula_names: Vec<String> = realized.formulas.keys().cloned().collect();
    let mut sorted_formula_names = formula_names;
    sorted_formula_names.sort();

    for formula_name in &sorted_formula_names {
        let formula = realized
            .formulas
            .get(formula_name)
            .expect("formula present");
        let env = realized.environment_for(automaton);

        // BoolDomain verdict (existing 2-valued evaluator).
        let bool_result = realized
            .context
            .evaluate_mu(automaton, &formula.formula, &env, None)
            .unwrap_or_else(|e| panic!("{name}/{formula_name}: BoolDomain eval failed: {e}"));

        // KleeneDomain verdict (via the trit evaluator that R.1 / R.3
        // wire up; the projection invariant is asserted below).
        let trit_result = realized
            .context
            .evaluate_mu_tri(automaton, &formula.formula, &env, None)
            .unwrap_or_else(|e| panic!("{name}/{formula_name}: KleeneDomain eval failed: {e}"));

        // R.3 invariant: on a Sharp-everywhere CLTS, KleeneDomain produces
        // **zero** `KleeneBot` verdicts and projects cell-by-cell to the
        // same Booleans BoolDomain would emit.
        let state_count = bool_result.len();
        for state in 0..state_count {
            let bool_holds = bool_result.get(state).map(|b| *b).unwrap_or(false);
            let trit_verdict = trit_result.verdict_at(state);
            match (bool_holds, trit_verdict) {
                (true, Trit::True) => {}
                (false, Trit::False) => {}
                _ => panic!(
                    "{name}/{formula_name}: R.3 projection invariant violated at state {state}: \
                     BoolDomain={bool_holds}, KleeneDomain={trit_verdict:?}. \
                     A Sharp-everywhere fixture must produce no Unknown verdicts and \
                     must project losslessly to the 2-valued evaluator's answer."
                ),
            }
        }

        // Snapshot the verdict at each state for the baseline JSON.
        let per_state: Vec<serde_json::Value> = (0..state_count)
            .map(|state| {
                json!({
                    "state": state,
                    "bool": bool_result.get(state).map(|b| *b).unwrap_or(false),
                    "kleene": match trit_result.verdict_at(state) {
                        Trit::True => "KleeneT",
                        Trit::False => "KleeneF",
                        Trit::Unknown => "KleeneBot",
                    }
                })
            })
            .collect();

        per_formula.push(json!({
            "formula": formula_name,
            "alternation_depth": formula.formula.alternation_depth(),
            "property_class": format!("{:?}", formula.formula.property_class()),
            "verdicts": per_state,
        }));
    }

    json!({
        "fixture": name,
        "automaton": automaton,
        "state_count": realized
            .context
            .clts(automaton)
            .map(|c| c.state_count())
            .unwrap_or(0),
        "formulas": per_formula,
    })
}

#[test]
fn r3_kleene_matches_bool_on_sharp_only_fixtures() {
    let runs = vec![
        run_fixture("linear_3", FIXTURE_LINEAR_3),
        run_fixture("singleton", FIXTURE_SINGLETON),
    ];

    let snapshot = serde_json::to_string_pretty(&json!({
        "_doc": "R.3 KMTS verdict baseline — Sharp-only CLTSes. \
                 KleeneDomain verdicts MUST equal BoolDomain projection for every cell. \
                 Generated by tests/r3_kleene_baseline.rs.",
        "runs": runs,
    }))
    .expect("serialize snapshot");

    if std::env::var("MUNUNU_R3_UPDATE_BASELINE").is_ok() {
        fs::write(baseline_path(), &snapshot)
            .unwrap_or_else(|e| panic!("write {}: {e}", baseline_path().display()));
        eprintln!(
            "R.3 UPDATE: wrote verdict baseline ({} fixtures) to {}",
            runs.len(),
            baseline_path().display()
        );
        return;
    }

    let committed = match fs::read_to_string(baseline_path()) {
        Ok(s) => s,
        Err(_) => {
            // First-run bootstrap.
            fs::write(baseline_path(), &snapshot)
                .unwrap_or_else(|e| panic!("write {}: {e}", baseline_path().display()));
            eprintln!(
                "R.3 BOOTSTRAP: verdict baseline did not exist; wrote {} fixtures to {}. \
                 Commit the file and re-run to enable regression mode.",
                runs.len(),
                baseline_path().display()
            );
            return;
        }
    };

    if committed.trim() != snapshot.trim() {
        // Find the first diff line for a useful failure message.
        let committed_lines: Vec<&str> = committed.lines().collect();
        let snapshot_lines: Vec<&str> = snapshot.lines().collect();
        let mut first_diff = None;
        for (i, (a, b)) in committed_lines
            .iter()
            .zip(snapshot_lines.iter())
            .enumerate()
        {
            if a != b {
                first_diff = Some((i + 1, *a, *b));
                break;
            }
        }
        let diff_msg = match first_diff {
            Some((line, expected, got)) => {
                format!("first diff at line {line}:\n  expected: {expected}\n  got:      {got}")
            }
            None => format!(
                "line counts differ: baseline={}, current={}",
                committed_lines.len(),
                snapshot_lines.len()
            ),
        };
        panic!(
            "R.3 verdict baseline diverged from {}.\n{diff_msg}\n\n\
             To accept the new baseline:\n\
               MUNUNU_R3_UPDATE_BASELINE=1 cargo test -p mununu-core --test r3_kleene_baseline",
            baseline_path().display()
        );
    }
}
