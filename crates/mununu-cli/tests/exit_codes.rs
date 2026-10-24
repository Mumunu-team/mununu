//! The process exit table every verb shares: `0` pass, `1` tool error, `2` violated, `3` unknown
//! under `--fail-on unknown`, `4` expectations unmet, `64` usage error.

use std::process::Command;

fn mununu(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mununu"))
        .args(args)
        .env("MUNUNU_QUIET", "1")
        .output()
        .expect("the binary runs")
}

#[test]
fn a_usage_error_exits_64_not_the_violated_code() {
    let out = mununu(&["sv", "verify-auto", "--no-such-flag"]);
    assert_eq!(out.status.code(), Some(64), "{out:?}");
    let out = mununu(&["no-such-command"]);
    assert_eq!(out.status.code(), Some(64), "{out:?}");
}

#[test]
fn help_and_version_exit_0() {
    assert_eq!(mununu(&["--help"]).status.code(), Some(0));
    assert_eq!(mununu(&["--version"]).status.code(), Some(0));
}

#[test]
fn context_eval_exits_2_on_a_refuted_formula_unless_the_gate_is_off() {
    let dir = std::env::temp_dir().join(format!("mununu-exit-codes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let model = dir.join("m.ctxdsl");
    std::fs::write(
        &model,
        "context m {
  alphabet { label a; }
  automata {
    automaton A {
      states { state S0 initial; state S1; }
      transitions { transition S0 -> S1 on label a; }
    }
  }
  mu_formulas {
    formula never_s1 { over A; body = nu X. (!S1 && [] X); }
    formula reach_s1 { over A; body = mu X. (S1 || <> X); }
  }
}
",
    )
    .unwrap();
    let path = model.to_str().unwrap();
    let eval = |formula: &str, extra: &[&str]| {
        let mut args = vec![
            "context",
            "eval",
            path,
            "--automaton",
            "A",
            "--formula",
            formula,
        ];
        args.extend_from_slice(extra);
        mununu(&args).status.code()
    };
    assert_eq!(eval("reach_s1", &[]), Some(0));
    assert_eq!(eval("never_s1", &[]), Some(2));
    assert_eq!(eval("never_s1", &["--fail-on", "none"]), Some(0));
}
