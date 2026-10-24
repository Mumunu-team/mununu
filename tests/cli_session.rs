use predicates::prelude::*;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use tempfile::tempdir;

fn write_simple_context(dir: &Path) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let context_path = dir.join("simple.ctxdsl");
    let sidecar_path = dir.join("simple_props.ctxdsl");
    let context_src = r"context simple {
    alphabet {
        label a;
    }
    automata {
        automaton Simple {
            states {
                state S0 initial;
                state S1;
            }
            transitions {
                transition S0 -> S1 on label a;
            }
        }
    }
}
";
    let sidecar_src = r"context simple_props {
    mu_formulas {
        formula reachability { over Simple; body = true; }
    }
}
";
    fs::write(&context_path, context_src)?;
    fs::write(&sidecar_path, sidecar_src)?;
    Ok((context_path, sidecar_path))
}

#[test]
fn context_merge_copies_outputs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let (context_path, sidecar_path) = write_simple_context(temp.path())?;
    let output_dir = temp.path().join("merged");

    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .current_dir(temp.path())
        .args([
            "context",
            "merge",
            context_path.to_str().unwrap(),
            sidecar_path.to_str().unwrap(),
            "--output",
            output_dir.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Merged context 'simple'"))
        .stdout(predicate::str::contains("Copied 2 file(s)"));

    assert!(output_dir.join("simple.ctxdsl").exists());
    assert!(output_dir.join("simple_props.ctxdsl").exists());
    Ok(())
}

#[test]
fn context_predicates_reports_results() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let (context_path, sidecar_path) = write_simple_context(temp.path())?;

    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .current_dir(temp.path())
        .args([
            "context",
            "predicates",
            context_path.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Automaton: Simple"))
        .stdout(predicate::str::contains("has_enabled_transition"))
        .stdout(predicate::str::contains("is_deadlock_state"));
    Ok(())
}

#[test]
fn context_summarize_with_print_structure_outputs_to_stdout() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let (context_path, sidecar_path) = write_simple_context(temp.path())?;

    let assert = assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .current_dir(temp.path())
        .args([
            "context",
            "summarize",
            context_path.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
            "--print-structure",
        ])
        .assert()
        .success();

    let output = String::from_utf8(assert.get_output().stdout.clone())?;
    assert!(output.contains("\"context\""));
    assert!(output.contains("Context Structure:"));
    assert!(output.contains("Automaton: Simple"));
    assert!(output.contains("States: 2"));

    Ok(())
}

#[test]
fn context_summarize_with_print_structure_outputs_to_file() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let (context_path, sidecar_path) = write_simple_context(temp.path())?;
    let structure_file = temp.path().join("structure.txt");

    let assert = assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .current_dir(temp.path())
        .args([
            "context",
            "summarize",
            context_path.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
            "--print-structure",
            structure_file.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(structure_file.exists());
    let structure_content = fs::read_to_string(&structure_file)?;
    assert!(structure_content.contains("Context Structure:"));
    assert!(structure_content.contains("Automaton: Simple"));
    assert!(structure_content.contains("States: 2"));

    let output = String::from_utf8(assert.get_output().stdout.clone())?;
    assert!(output.contains("\"context\""));
    assert!(output.contains("Context structure written to"));

    Ok(())
}

#[test]
fn context_eval_with_print_structure_outputs_structure() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let (context_path, sidecar_path) = write_simple_context(temp.path())?;

    let assert = assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .current_dir(temp.path())
        .args([
            "context",
            "eval",
            context_path.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
            "--formula",
            "reachability",
            "--automaton",
            "Simple",
            "--print-structure",
        ])
        .assert()
        .success();

    let output = String::from_utf8(assert.get_output().stdout.clone())?;
    assert!(output.contains("Context Structure:"));
    assert!(output.contains("Automaton: Simple"));
    assert!(output.contains("Formula 'reachability'"));

    Ok(())
}

#[test]
fn context_eval_reports_satisfying_states() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let (context_path, sidecar_path) = write_simple_context(temp.path())?;

    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .current_dir(temp.path())
        .args([
            "context",
            "eval",
            context_path.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
            "--formula",
            "reachability",
            "--automaton",
            "Simple",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("States satisfying: 2/2"))
        .stdout(predicate::str::contains("Initial states satisfying: 1/1"));
    Ok(())
}

// ---------------------------------------------------------------------------
// `mununu verify` — the general N-source verification CLI (A2.5).
// ---------------------------------------------------------------------------

fn write_two_source_verify_project(dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let light_ctxdsl = r#"
context Light {
    alphabet { label tick_light; }
    automata {
        automaton Light {
            states { state green initial; state yellow; state red; }
            transitions {
                transition green -> yellow on label tick_light;
                transition yellow -> red on label tick_light;
                transition red -> green on label tick_light;
            }
        }
    }
}
"#;
    let gate_ctxdsl = r#"
context Gate {
    alphabet { label tick_gate; }
    automata {
        automaton Gate {
            states { state closed initial; state open; }
            transitions {
                transition closed -> open on label tick_gate;
                transition open -> closed on label tick_gate;
            }
        }
    }
}
"#;
    fs::write(dir.join("light.ctxdsl"), light_ctxdsl)?;
    fs::write(dir.join("gate.ctxdsl"), gate_ctxdsl)?;

    let verify_toml = r#"
[project]
name = "Demo"

[[sources]]
id = "light"
adapter = "ctxdsl"
files = ["light.ctxdsl"]

[[sources]]
id = "gate"
adapter = "ctxdsl"
files = ["gate.ctxdsl"]

[alphabet]
strategy = "direct"

[composition]
semantics = "asynchronous"
members = ["light", "gate"]
name = "System"

[[properties]]
name = "no_deadlock"
template = "no_deadlock"
over = "System"

[[properties]]
name = "always_true"
formula = "true"
over = "System"
"#;
    let toml_path = dir.join("verify.toml");
    fs::write(&toml_path, verify_toml)?;
    Ok(toml_path)
}

#[test]
fn verify_reports_satisfied_properties_in_human_format() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let toml_path = write_two_source_verify_project(temp.path())?;

    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args(["verify", toml_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("verify report — project `Demo`"))
        .stdout(predicate::str::contains("asynchronous System"))
        .stdout(predicate::str::contains("light"))
        .stdout(predicate::str::contains("gate"))
        .stdout(predicate::str::contains("always_true: SATISFIED"))
        .stdout(predicate::str::contains("[inline,"));
    Ok(())
}

#[test]
fn verify_json_output_round_trips_through_serde() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let toml_path = write_two_source_verify_project(temp.path())?;

    let output = assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args(["verify", toml_path.to_str().unwrap(), "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(parsed["project"], "Demo");
    let sources = parsed["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    let verdicts = parsed["property_verdicts"].as_array().unwrap();
    assert_eq!(verdicts.len(), 2);
    // Both properties should be satisfied — `always_true` trivially,
    // `no_deadlock` because every state has at least one enabled
    // transition under asynchronous composition.
    for v in verdicts {
        assert_eq!(v["outcome"], "holds");
    }
    Ok(())
}

#[test]
fn verify_strict_mode_fails_on_violation() -> Result<(), Box<dyn Error>> {
    // Pin a property to `false` so it's universally violated; --strict
    // should then return a non-zero exit code.
    let temp = tempdir()?;
    let _ = write_two_source_verify_project(temp.path())?;
    let bad_toml = r#"
[project]
name = "Bad"

[[sources]]
id = "light"
adapter = "ctxdsl"
files = ["light.ctxdsl"]

[composition]
semantics = "synchronous"
members = ["light"]
name = "Sys"

[[properties]]
name = "impossible"
formula = "false"
over = "Sys"
"#;
    let toml_path = temp.path().join("bad_verify.toml");
    fs::write(&toml_path, bad_toml)?;

    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args(["verify", toml_path.to_str().unwrap(), "--strict"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("impossible: VIOLATED"));
    Ok(())
}

#[test]
fn verify_config_validation_failure_surfaces_clearly() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let bad_toml = r#"
[project]
name = "Bad"

[[sources]]
id = "x"
adapter = "ctxdsl"
files = ["doesnt_matter.ctxdsl"]

[composition]
semantics = "lockstep"   # invalid semantics
members = ["x"]
"#;
    let toml_path = temp.path().join("bad_verify.toml");
    fs::write(&toml_path, bad_toml)?;

    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args(["verify", toml_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("validation issue"));
    Ok(())
}

// ---------------------------------------------------------------------------
// `mununu codesign reconcile-labels` — Doc C §C.5 hard-gate CLI (PR #54).
// ---------------------------------------------------------------------------

fn write_labels_array(dir: &Path, name: &str, labels: &[&str]) -> Result<PathBuf, Box<dyn Error>> {
    let path = dir.join(name);
    let body = serde_json::to_string(labels)?;
    fs::write(&path, body)?;
    Ok(path)
}

#[test]
fn codesign_reconcile_labels_reports_clean_match() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let fw = write_labels_array(
        temp.path(),
        "fw.json",
        &["rd_status_tx_busy", "wr_ctrl_tx_start"],
    )?;
    let p = write_labels_array(
        temp.path(),
        "p.json",
        &["rd_status_tx_busy", "wr_ctrl_tx_start"],
    )?;
    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args([
            "codesign",
            "reconcile-labels",
            fw.to_str().unwrap(),
            p.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("alphabets reconcile"))
        .stdout(predicate::str::contains("wr_ctrl_tx_start"))
        .stdout(predicate::str::contains("rd_status_tx_busy"));
    Ok(())
}

#[test]
fn codesign_reconcile_labels_reports_firmware_only_mismatch() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let fw = write_labels_array(
        temp.path(),
        "fw.json",
        &["wr_ctrl_tx_start", "wr_data_byte"],
    )?;
    let p = write_labels_array(temp.path(), "p.json", &["wr_ctrl_tx_start"])?;
    assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args([
            "codesign",
            "reconcile-labels",
            fw.to_str().unwrap(),
            p.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("label-alphabet mismatch"))
        .stderr(predicate::str::contains("firmware-only"))
        .stderr(predicate::str::contains("wr_data_byte"));
    Ok(())
}

#[test]
fn codesign_reconcile_labels_emits_json_on_request() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let fw = write_labels_array(temp.path(), "fw.json", &["wr_a"])?;
    let p = write_labels_array(temp.path(), "p.json", &["wr_a"])?;
    let assert = assert_cmd::cargo::cargo_bin_cmd!("mununu")
        .args([
            "codesign",
            "reconcile-labels",
            fw.to_str().unwrap(),
            p.to_str().unwrap(),
            "--format",
            "json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    let parsed: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(parsed["shared"], serde_json::json!(["wr_a"]));
    assert_eq!(parsed["mismatch"], serde_json::Value::Null);
    Ok(())
}
