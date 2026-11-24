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
    // Pin a property to `false` so it's universally violated: the product CLI's verdict gate
    // exits 2 on a violation by default (`--fail-on violated`). The legacy root binary needed
    // `--strict` for that; it was removed on 2026-11-09 and the gate is the contract now.
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
        .args(["verify", toml_path.to_str().unwrap()])
        .assert()
        .code(2)
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

// ---------------------------------------------------------------------------------------------
// mununu#1201 — what each `sv` verb PRINTS about the clocks its lift merged.
//
// The API's rows read each route's serialised response
// (`e2e_x1181_every_sv_route_reports_the_merged_clock_domains`), and a policy scan holds that no
// product code reads a lift's result and drops its notes. Neither sees what a command prints.
// These two tests do, over the binary: one row per verb and mode that takes
// `--merge-clock-domains`.
// ---------------------------------------------------------------------------------------------

/// Two clock domains: `seen` on `clk_a`, the three-state `st` on `clk_b` (the design of the
/// API's rows).
const TWO_CLOCKS: &str = "\
module top(input logic clk_a, input logic clk_b, input logic rst, input logic req,
           output logic gnt);
  logic seen;
  logic [1:0] st;
  always_ff @(posedge clk_a)
    if (rst) seen <= 1'b0;
    else seen <= req;
  always_ff @(posedge clk_b)
    if (rst) st <= 2'd0;
    else case (st)
      2'd0: st <= seen ? 2'd1 : 2'd0;
      2'd1: st <= 2'd2;
      default: st <= 2'd0;
    endcase
  assign gnt = (st == 2'd2);
  a_st_bounded: assert property (@(posedge clk_b) st != 2'd3);
endmodule
";

/// The same design on one clock: the lift merges nothing, so no report mentions a merge.
const ONE_CLOCK: &str = "\
module top(input logic clk_a, input logic rst, input logic req,
           output logic gnt);
  logic seen;
  logic [1:0] st;
  always_ff @(posedge clk_a)
    if (rst) seen <= 1'b0;
    else seen <= req;
  always_ff @(posedge clk_a)
    if (rst) st <= 2'd0;
    else case (st)
      2'd0: st <= seen ? 2'd1 : 2'd0;
      2'd1: st <= 2'd2;
      default: st <= 2'd0;
    endcase
  assign gnt = (st == 2'd2);
  a_st_bounded: assert property (@(posedge clk_a) st != 2'd3);
endmodule
";

/// The note's wire values: its `kind`, and the `level` of a caveat on what a verdict is worth.
const MERGED_KIND: &str = "clock-domains-merged";
const CAVEAT_LEVEL: &str = "soundness-caveat";

/// Where a command's output carries what the lift merged.
#[derive(Clone, Copy)]
enum MergeReport {
    /// JSON: the `lift_notes` array, absent when the lift has nothing to say.
    LiftNotes,
    /// JSON: the run's `notes` array (`sv verify-auto`, which has other notes too).
    Notes,
    /// Text: one `  note [<level>] <kind>: <summary>` line per note.
    PlanText,
    /// JSON: the `domains` array (`sv cdc-lint`, whose report is the clock domains themselves).
    Domains,
}

struct MergeRow {
    name: &'static str,
    verb: &'static str,
    args: &'static [&'static str],
    report: MergeReport,
}

/// One row per verb and mode. `every_sv_verb_that_takes_merge_clock_domains_has_a_row` holds the
/// verbs of this table equal to the verbs whose help lists the flag.
const MERGE_ROWS: &[MergeRow] = &[
    MergeRow {
        name: "verify-liveness (request/grant)",
        verb: "verify-liveness",
        args: &[
            "--request",
            "seen == 1",
            "--grant",
            "st == 2",
            "--json",
            "--fail-on",
            "none",
        ],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "verify-liveness (under fairness)",
        verb: "verify-liveness",
        args: &[
            "--request",
            "seen == 1",
            "--grant",
            "st == 2",
            "--fairness",
            "seen == 1",
            "--json",
            "--fail-on",
            "none",
        ],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "verify-liveness (--response)",
        verb: "verify-liveness",
        args: &[
            "--response",
            "seen == 1 => st == 2",
            "--json",
            "--fail-on",
            "none",
        ],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "verify-recoverability",
        verb: "verify-recoverability",
        args: &["--target", "st == 0", "--json", "--fail-on", "none"],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "verify-recoverability (--refine)",
        verb: "verify-recoverability",
        args: &[
            "--target",
            "st == 0",
            "--refine",
            "--json",
            "--fail-on",
            "none",
        ],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "check-fsm",
        verb: "check-fsm",
        args: &["--fail-on", "none"],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "check-fsm (--liveness)",
        verb: "check-fsm",
        args: &["--liveness", "--fail-on", "none"],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "lint",
        verb: "lint",
        args: &["--fail-on", "none"],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "mutate (--list)",
        verb: "mutate",
        args: &["--list"],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "mutate (one mutation)",
        verb: "mutate",
        args: &[
            "--mutation",
            "stick:st",
            "--engine",
            "exact-symbolic",
            "--fail-on",
            "none",
        ],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "verify-auto",
        verb: "verify-auto",
        args: &["--json", "--fail-on", "none"],
        report: MergeReport::Notes,
    },
    MergeRow {
        name: "verify-auto (--plan-only, JSON)",
        verb: "verify-auto",
        args: &["--plan-only", "--json"],
        report: MergeReport::LiftNotes,
    },
    MergeRow {
        name: "verify-auto (--plan-only, text)",
        verb: "verify-auto",
        args: &["--plan-only"],
        report: MergeReport::PlanText,
    },
    MergeRow {
        name: "cdc-lint",
        verb: "cdc-lint",
        args: &["--fail-on", "none"],
        report: MergeReport::Domains,
    },
];

/// Whether `stdout` reports a merge of clock domains, read where the row's command carries it.
/// `Err` when the output is not what the row's command prints (not JSON, a field of another
/// type).
fn reports_a_merge(report: MergeReport, stdout: &str) -> Result<bool, String> {
    let json = || -> Result<serde_json::Value, String> {
        serde_json::from_str(stdout).map_err(|e| format!("stdout is not JSON ({e})"))
    };
    let is_the_caveat =
        |n: &serde_json::Value| n["kind"] == MERGED_KIND && n["level"] == CAVEAT_LEVEL;
    let notes_under = |key: &str| -> Result<Option<Vec<serde_json::Value>>, String> {
        match json()?.get(key) {
            None => Ok(None),
            Some(serde_json::Value::Array(notes)) => Ok(Some(notes.clone())),
            Some(other) => Err(format!("`{key}` is not an array: {other}")),
        }
    };
    match report {
        MergeReport::LiftNotes => match notes_under("lift_notes")? {
            None => Ok(false),
            // The key is absent when there is nothing to say: present, it holds the caveat.
            Some(notes) if notes.iter().any(is_the_caveat) => Ok(true),
            Some(notes) => Err(format!(
                "`lift_notes` is present without the caveat: {notes:?}"
            )),
        },
        MergeReport::Notes => {
            let notes = notes_under("notes")?.ok_or("the report has no `notes`")?;
            Ok(notes.iter().any(is_the_caveat))
        }
        MergeReport::PlanText => {
            let head = format!("note [{CAVEAT_LEVEL}] {MERGED_KIND}:");
            Ok(stdout.lines().any(|l| l.trim_start().starts_with(&head)))
        }
        MergeReport::Domains => {
            let domains = notes_under("domains")?.ok_or("the report has no `domains`")?;
            Ok(domains.len() > 1)
        }
    }
}

/// The `sv` verbs whose help lists `--merge-clock-domains`, read from the binary.
fn sv_verbs_that_take_the_merge_flag() -> Result<Vec<String>, Box<dyn Error>> {
    let help_of = |args: &[&str]| -> Result<String, Box<dyn Error>> {
        let out = assert_cmd::cargo::cargo_bin_cmd!("mununu")
            .args(args)
            .assert()
            .success();
        Ok(String::from_utf8(out.get_output().stdout.clone())?)
    };
    // clap lists the subcommands under `Commands:`, one per line at two spaces, to the next
    // blank line.
    let sv_help = help_of(&["sv", "-h"])?;
    let verbs: Vec<String> = sv_help
        .lines()
        .skip_while(|l| l.trim() != "Commands:")
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|verb| *verb != "help")
        .map(str::to_string)
        .collect();
    let mut with_flag = Vec::new();
    for verb in verbs {
        let help = help_of(&["sv", verb.as_str(), "-h"])?;
        if help
            .lines()
            .any(|l| l.trim_start().starts_with("--merge-clock-domains"))
        {
            with_flag.push(verb);
        }
    }
    Ok(with_flag)
}

/// Whether yosys with `read_slang` is here. The probe of the core crate's adapter tests
/// (`crates/mununu-core/tests/adapters/support.rs`), repeated because one crate's test support
/// is not importable from another's tests; under `MUNUNU_REQUIRE_TOOLS=1` an absent tool fails
/// instead of skipping (mununu#981).
fn slang_lift_available() -> bool {
    let mut cmd = std::process::Command::new("yosys");
    if let Some(plugin) = std::env::var("MUNUNU_YOSYS_SLANG_PLUGIN")
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        cmd.arg("-m").arg(plugin);
    }
    let present = cmd
        .args(["-p", "help read_slang"])
        .output()
        .map(|o| {
            // `help <unknown>` exits 0 and prints "No such command or cell type" instead.
            o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("No such command")
        })
        .unwrap_or(false);
    mununu_core::adapter::require_tool("yosys with read_slang", present)
}

/// mununu#1201 — the table has a row for every verb that takes the flag, and no row for a verb
/// that does not. Needs no tool: it reads the binary's help, so it runs on every host.
#[test]
fn every_sv_verb_that_takes_merge_clock_domains_has_a_row() -> Result<(), Box<dyn Error>> {
    let mut with_flag = sv_verbs_that_take_the_merge_flag()?;
    with_flag.sort();
    let mut with_row: Vec<String> = MERGE_ROWS.iter().map(|r| r.verb.to_string()).collect();
    with_row.sort();
    with_row.dedup();
    assert_eq!(
        with_flag, with_row,
        "left: the `sv` verbs whose help lists --merge-clock-domains; right: the verbs of \
         MERGE_ROWS. A verb that gains the flag gets a row per mode."
    );
    Ok(())
}

/// mununu#1201 — under `--merge-clock-domains`, what each `sv` verb prints says that two clock
/// domains were merged when they were, and says nothing of a merge on a one-clock design.
#[test]
fn every_sv_verb_under_merge_clock_domains_prints_the_merged_domains() -> Result<(), Box<dyn Error>>
{
    if !slang_lift_available() {
        eprintln!("skipped: yosys with read_slang is not available on this host");
        return Ok(());
    }
    let temp = tempdir()?;
    let mut wrong = Vec::new();
    for (file, source, merged) in [("two.sv", TWO_CLOCKS, true), ("one.sv", ONE_CLOCK, false)] {
        let path = temp.path().join(file);
        fs::write(&path, source)?;
        for row in MERGE_ROWS {
            let output = assert_cmd::cargo::cargo_bin_cmd!("mununu")
                .current_dir(temp.path())
                .args(["sv", row.verb, path.to_str().unwrap(), "--top", "top"])
                .args(["--merge-clock-domains", "--quiet"])
                .args(row.args)
                .output()?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let found = if output.status.success() {
                reports_a_merge(row.report, &stdout)
            } else {
                Err(format!(
                    "exit {:?}: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ))
            };
            if found != Ok(merged) {
                wrong.push(format!(
                    "{} on {file}: expected a merge reported = {merged}, got {found:?}",
                    row.name
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} runs:\n{}",
        wrong.len(),
        2 * MERGE_ROWS.len(),
        wrong.join("\n")
    );
    Ok(())
}
