//! XL.1 — `slang` SVA front-end subprocess wrapper (discovery + invocation).
//!
//! Track H's SystemVerilog-assertion verification path uses the open-source
//! [`slang`](https://github.com/MikePopoloski/slang) CLI as a discovered
//! subprocess to parse SVA out of SV source into a structured AST: mununu shells
//! to `slang --ast-json` and consumes the JSON. This module is the discovery +
//! invocation half (the same pattern as [`crate::adapter::cvc5`] / sv2v / Yosys);
//! the JSON parse + the Tier-1 SVA → mu-calculus translator are the XL.1 follow-up.
//!
//! **Why slang (XL.0 decision).** The target corpus's SVA is OpenTitan
//! `` `ASSERT ``-macro-wrapped, which needs a real SV preprocessor + full SVA
//! parser — tree-sitter can't preprocess and sv2v silently drops concurrent
//! assertions. slang (MIT) preprocesses macros/includes and serialises full SVA
//! via `--ast-json`. Confirmed on the real CLI: prim_arbiter's 13 `` `ASSERT ``s +
//! their `|->` implications serialise as structured nodes — see
//! `.claude/plans/measurements/XL-0-sva-parser-spike-2026-06-26.md`.
//!
//! **Licensing.** Subprocess invocation is "mere aggregation": slang's license
//! does not contaminate mununu's (see `docs/external-tools.md`). slang is
//! contributor-installed (CI uses the Linux prebuilt), never bundled.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::adapter::{AdapterError, AdapterErrorKind};

pub mod assertion_rewrite;
pub mod assume_guarantee;
pub mod expectations;
pub mod expr_ir;
pub mod extract;
pub mod prim_stubs;
pub(crate) mod report_consistency;
pub mod translate;
pub mod verify_auto;

/// Handle to a discovered slang binary + its parsed version (diagnostic only;
/// mununu does not gate on version). Mirrors [`crate::adapter::cvc5::Cvc5Bin`].
#[derive(Debug, Clone)]
pub struct SlangBin {
    /// Resolved path — a bare `slang` (found on `$PATH`) or the absolute path
    /// from `MUNUNU_SLANG_PATH`.
    pub path: PathBuf,
    /// Parsed version (e.g. `11.0.0+7ddf405`); `"<unparseable>"` when the
    /// `--version` output doesn't match — discovery still succeeds.
    pub version: String,
}

/// Locate a usable slang binary + probe its version.
///
/// Discovery: `MUNUNU_SLANG_PATH` env var, else `slang` on `$PATH`. Returns
/// `Err(AdapterError { kind: UnsupportedConstruct, .. })` when the binary is
/// absent or the version probe fails — callers fall back gracefully (the
/// SVA-extraction feature degrades; model verification + mununu-annotation
/// properties are unaffected), the cvc5 precedent.
pub fn locate_slang() -> Result<SlangBin, AdapterError> {
    // AR-GO-2 — shared locate body; see `crate::adapter::locate_tool`.
    let (path, version) = crate::adapter::locate_tool(
        "MUNUNU_SLANG_PATH",
        "slang",
        "slang",
        "Set MUNUNU_SLANG_PATH or install slang \
         (https://github.com/MikePopoloski/slang — Linux + macOS-arm64 prebuilts on the \
         releases page; build from source elsewhere). See docs/external-tools.md.",
        parse_slang_version,
    )?;
    Ok(SlangBin { path, version })
}

/// Extract the version token from `slang --version` output, e.g.
/// `slang version 11.0.0+7ddf405` → `11.0.0+7ddf405`.
pub fn parse_slang_version(stdout: &str) -> Option<String> {
    let mut it = stdout.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "version" {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

/// The slang argument list for an `--ast-json` run — pure, so the override threading is unit
/// testable without the binary.
///
/// `--quiet` suppresses slang's "Top level design units" / build-summary banner so stdout is
/// *pure* JSON (the `-` sink). Without it the banner prefixes the JSON and downstream
/// `serde_json` parsing fails at column 1. A `MODULE.NAME` override is passed by its bare `NAME`:
/// slang's `-G` is top-scoped, the same convention the `read_slang` lift uses
/// (`yosys::build_script`); an override that still does not reach the elaborated design is
/// caught after extraction (`verify_auto::prepare_model`), never dropped.
pub fn ast_json_args(
    files: &[PathBuf],
    include_dirs: &[PathBuf],
    params: &[(String, String)],
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["--ast-json", "-", "--quiet", "--single-unit"]
        .iter()
        .map(Into::into)
        .collect();
    for (lhs, value) in params {
        let name = lhs.rsplit('.').next().unwrap_or(lhs);
        args.push("-G".into());
        args.push(format!("{name}={value}").into());
    }
    for d in include_dirs {
        args.push("-I".into());
        args.push(d.into());
    }
    for f in files {
        args.push(f.into());
    }
    args
}

/// Run `slang --ast-json - <files> -I <dirs> --single-unit` and return the
/// emitted AST JSON (the `-` sink writes JSON to stdout).
///
/// slang emits the AST **even when the design has elaboration errors** (e.g. an
/// unprovided package) — exactly what SVA extraction needs, since the assertion
/// syntax is present regardless. So a non-zero exit is NOT fatal as long as JSON
/// was produced (stderr is surfaced via `tracing`); only empty stdout (no JSON)
/// is an error.
pub fn run_ast_json(
    bin: &SlangBin,
    files: &[PathBuf],
    include_dirs: &[PathBuf],
) -> Result<String, AdapterError> {
    run_ast_json_with_params(bin, files, include_dirs, &[])
}

/// mununu#722 — the extraction with the SOURCE TEXT a hierarchical reference was written with.
///
/// slang's JSON names a `HierarchicalValue` by its DECLARATION (`"<addr> err_o"`), which several
/// instances share, so a reference into a module instantiated more than once cannot say which
/// instance it means. `--ast-json-source-info` gives every node its source span — but a span
/// inside a macro expansion collapses to the macro's call site, and OpenTitan writes every such
/// reference through a macro (`ASSERT_PRIM_COUNT_ERROR_TRIGGER_ALERT(…, u_core.u_cnt, …)`). So
/// the unit is PREPROCESSED first (`slang -E`, one file, macros expanded, `include`s inlined, no
/// `line` directives) and the AST is dumped from that text with source info: the span then slices
/// the path verbatim (`u_core.gen_fifo.u_cnt.err_o`). Returns `(json, preprocessed text)`; the
/// translator reads the text only for a reference it cannot resolve otherwise.
///
/// The preprocessed unit is written beside the first staged file (`__mununu_preprocessed.sv`) so
/// relative `include` paths keep resolving the same way.
pub fn run_ast_json_with_source(
    bin: &SlangBin,
    files: &[PathBuf],
    include_dirs: &[PathBuf],
    params: &[(String, String)],
) -> Result<(String, String), AdapterError> {
    let mut pp = Command::new(&bin.path);
    pp.args(preprocess_args(files, include_dirs));
    let pp_out = run_slang_bounded(&mut pp, bin, "-E")?;
    let text = String::from_utf8_lossy(&pp_out.stdout).to_string();
    if text.trim().is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/slang: `slang -E` produced no output (status {}). stderr: {}",
                pp_out.status,
                String::from_utf8_lossy(&pp_out.stderr).trim()
            ),
            location: None,
        });
    }
    let dir = files
        .first()
        .and_then(|f| f.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let pp_path = dir.join("__mununu_preprocessed.sv");
    std::fs::write(&pp_path, &text).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/slang: could not write the preprocessed unit `{}`: {e}",
            pp_path.display()
        ),
        location: None,
    })?;
    let mut args = ast_json_args(std::slice::from_ref(&pp_path), include_dirs, params);
    args.push("--ast-json-source-info".into());
    let mut cmd = Command::new(&bin.path);
    cmd.args(args);
    let json = finish_ast_json(run_slang_bounded(&mut cmd, bin, "--ast-json")?)?;
    Ok((json, text))
}

/// `slang -E` over the same unit the AST step compiles: one preprocessed text on stdout.
pub fn preprocess_args(files: &[PathBuf], include_dirs: &[PathBuf]) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-E", "--quiet", "--single-unit"]
        .iter()
        .map(Into::into)
        .collect();
    for d in include_dirs {
        args.push("-I".into());
        args.push(d.into());
    }
    for f in files {
        args.push(f.into());
    }
    args
}

/// Run one slang command under the lift timeout (mununu#504): a timeout is a hard error here,
/// since slang is what DISCOVERS the properties — there is no property list to degrade into.
fn run_slang_bounded(
    cmd: &mut Command,
    bin: &SlangBin,
    what: &str,
) -> Result<std::process::Output, AdapterError> {
    let outcome = match crate::adapter::lift_timeout() {
        Some(t) => crate::adapter::run_with_timeout(cmd, None, t),
        None => cmd.output().map(|o| {
            Some((
                o.status,
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ))
        }),
    }
    .map_err(|e| AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!(
            "adapter/slang: failed to run `{} {what}`: {e}",
            bin.path.display()
        ),
        location: None,
    })?;
    let Some((status, stdout, stderr_s)) = outcome else {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/slang: `slang {what}` exceeded the lift timeout and was killed. slang runs \
                 before the property list exists, so there is nothing to report a partial verdict \
                 for. Raise or disable the cap with {}=<ms> (0 disables).",
                crate::adapter::LIFT_TIMEOUT_ENV
            ),
            location: None,
        });
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.into_bytes(),
        stderr: stderr_s.into_bytes(),
    })
}

/// The JSON of a finished `--ast-json` run: empty stdout is the error, a non-zero status with JSON
/// is not (see [`run_ast_json`]).
fn finish_ast_json(output: std::process::Output) -> Result<String, AdapterError> {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/slang: `slang --ast-json` produced no JSON (status {}). stderr: {}",
                output.status,
                stderr.trim()
            ),
            location: None,
        });
    }
    if !output.status.success() {
        tracing::warn!(
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "slang --ast-json exited non-zero but produced JSON; continuing"
        );
    }
    Ok(stdout)
}

/// [`run_ast_json`] elaborating the design under `--param` overrides.
///
/// mununu#717 — the SVA extraction must elaborate the design the way the lift does. It ran with
/// no override, so slang elaborated every parameter at its DEFAULT while the lift applied
/// `--param`: a bound port then read a default-width signal the lifted model did not have (the
/// property was skipped), and a parameter folded into an atom took its default value (a verdict
/// about a different design). The overrides are passed as slang's top-scoped `-G NAME=VALUE`,
/// exactly as the `read_slang` lift passes them, so the two front ends agree by construction.
pub fn run_ast_json_with_params(
    bin: &SlangBin,
    files: &[PathBuf],
    include_dirs: &[PathBuf],
    params: &[(String, String)],
) -> Result<String, AdapterError> {
    let mut cmd = Command::new(&bin.path);
    cmd.args(ast_json_args(files, include_dirs, params));

    // mununu#504 — bounded. This ran as a bare `.output()`. slang runs FIRST, before anything is
    // known about the design, so a hang here loses the run with no output and nothing to report.
    let outcome = match crate::adapter::lift_timeout() {
        Some(t) => crate::adapter::run_with_timeout(&mut cmd, None, t),
        None => cmd.output().map(|o| {
            Some((
                o.status,
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ))
        }),
    }
    .map_err(|e| AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!(
            "adapter/slang: failed to run `{} --ast-json`: {e}",
            bin.path.display()
        ),
        location: None,
    })?;
    // A slang timeout stays a hard error, unlike yosys/sv2v: slang is what DISCOVERS the
    // properties, so at this point there is no property list to degrade into an all-`unknown`
    // report. The honest outcome is a clear failure naming the cap.
    let Some((status, stdout, stderr_s)) = outcome else {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/slang: `slang --ast-json` exceeded the lift timeout and was killed. \
                 slang runs before the property list exists, so there is nothing to report a \
                 partial verdict for. Raise or disable the cap with {}=<ms> (0 disables).",
                crate::adapter::LIFT_TIMEOUT_ENV
            ),
            location: None,
        });
    };
    let output = std::process::Output {
        status,
        stdout: stdout.into_bytes(),
        stderr: stderr_s.into_bytes(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/slang: `slang --ast-json` produced no JSON (status {}). stderr: {}",
                output.status,
                stderr.trim()
            ),
            location: None,
        });
    }
    if !output.status.success() {
        tracing::warn!(
            status = %output.status,
            "adapter/slang: slang reported elaboration diagnostics; AST JSON still emitted \
             (assertion syntax is present, which is what SVA extraction consumes)"
        );
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mununu#717 — the SVA extraction elaborates under the same overrides the lift applies,
    /// with a scoped `MODULE.NAME` passed by its bare name (slang's `-G` is top-scoped), the
    /// convention of the `read_slang` lift.
    #[test]
    fn ast_json_args_pass_each_param_override_as_a_g_flag() {
        let args: Vec<String> = ast_json_args(
            &[PathBuf::from("a.sv")],
            &[PathBuf::from("inc")],
            &[("W".into(), "4".into()), ("cnt.D".into(), "2".into())],
        )
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
        assert_eq!(
            args,
            [
                "--ast-json",
                "-",
                "--quiet",
                "--single-unit",
                "-G",
                "W=4",
                "-G",
                "D=2",
                "-I",
                "inc",
                "a.sv"
            ]
        );
        assert!(
            !ast_json_args(&[PathBuf::from("a.sv")], &[], &[])
                .iter()
                .any(|a| a == "-G"),
            "no override, no -G"
        );
    }

    #[test]
    fn parse_slang_version_extracts_token() {
        assert_eq!(
            parse_slang_version("slang version 11.0.0+7ddf405").as_deref(),
            Some("11.0.0+7ddf405")
        );
        assert_eq!(
            parse_slang_version("This is slang version 12.1.0").as_deref(),
            Some("12.1.0")
        );
        assert_eq!(parse_slang_version("garbage output no v"), None);
    }

    #[test]
    fn locate_slang_returns_structured_error_when_binary_absent() {
        let original = std::env::var("MUNUNU_SLANG_PATH").ok();
        // SAFETY: env vars are process-global; set + restore to avoid leaking
        // the bogus path to other tests (mirrors the cvc5 test).
        unsafe {
            std::env::set_var(
                "MUNUNU_SLANG_PATH",
                "/nonexistent/path/to/slang/definitely/not/here",
            );
        }
        let result = locate_slang();
        unsafe {
            match original {
                Some(v) => std::env::set_var("MUNUNU_SLANG_PATH", v),
                None => std::env::remove_var("MUNUNU_SLANG_PATH"),
            }
        }
        assert!(
            result.is_err(),
            "locate_slang MUST return Err when the binary is absent; got {result:?}"
        );
        let err = result.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        assert!(
            err.message.contains("slang"),
            "error message MUST mention slang for diagnosability; got: {}",
            err.message
        );
    }

    #[test]
    #[ignore = "requires the slang CLI (MUNUNU_SLANG_PATH or $PATH); run with --ignored"]
    fn run_ast_json_emits_assertions_when_slang_available() {
        let bin = locate_slang().expect("slang available for the integration test");
        let dir = std::env::temp_dir().join("mununu_slang_xl1_test");
        std::fs::create_dir_all(&dir).unwrap();
        let sv = dir.join("tier1.sv");
        std::fs::write(
            &sv,
            "module tier1 (input logic clk, input logic a, input logic b);\n\
             ap_impl: assert property (@(posedge clk) a |-> b);\n\
             endmodule\n",
        )
        .unwrap();
        let json = run_ast_json(&bin, &[sv], &[]).expect("ast-json runs");
        assert!(
            json.contains("ConcurrentAssertion"),
            "slang --ast-json must surface the concurrent assertion; got {} bytes",
            json.len()
        );
        assert!(
            json.contains("OverlappedImplication"),
            "the `|->` must serialise as an implication op"
        );
    }
}

/// Where every port, net and variable is declared, from slang's own AST with
/// `--ast-json-source-info` (2026-10-29): the one source of declaration lines that does not depend
/// on the lift's front end — yosys-slang stamps them on the netlist only in recent builds, yosys's
/// `read_verilog` does, sv2v never. Runs on the ORIGINAL staged files (the SVA extraction's own
/// run works on a preprocessed copy whose lines are shifted). Names are spelled as the lift
/// spells them: the top instance's members bare, a nested instance's as `inst.name`, a generate
/// block's `gen[k]` as `gen$k`.
pub fn run_declarations(
    bin: &SlangBin,
    files: &[PathBuf],
    include_dirs: &[PathBuf],
    params: &[(String, String)],
) -> Result<Vec<(String, crate::trace::SourceLoc)>, AdapterError> {
    let mut args = ast_json_args(files, include_dirs, params);
    args.push("--ast-json-source-info".into());
    let mut cmd = Command::new(&bin.path);
    cmd.args(&args);
    let out = run_slang_bounded(&mut cmd, bin, "--ast-json-source-info")?;
    let json = String::from_utf8_lossy(&out.stdout);
    if json.trim().is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/slang: the declarations run produced no AST (status {}). stderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            location: None,
        });
    }
    Ok(declarations_from_ast(&json))
}

/// The declarations of a slang AST dumped with `--ast-json-source-info`: `Root → members →
/// Instance → body (InstanceBody) → members`, where a `Port` / `Variable` / `Net` carries
/// `source_file` and `source_line`. The first declaration of a name wins (a port appears as both a
/// `Port` and a `Variable` / `Net` on the same line). Pure, so the shape is pinned by a test.
pub fn declarations_from_ast(json: &str) -> Vec<(String, crate::trace::SourceLoc)> {
    use crate::trace::SourceLoc;
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out: Vec<(String, SourceLoc)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    fn segment(name: &str) -> String {
        // `gen[3]` → `gen$3`, the lift's spelling (mununu#713).
        name.replace('[', "$").replace(']', "")
    }
    fn walk(
        node: &serde_json::Value,
        prefix: &str,
        top: bool,
        out: &mut Vec<(String, SourceLoc)>,
        seen: &mut std::collections::BTreeSet<String>,
    ) {
        let Some(obj) = node.as_object() else {
            if let Some(items) = node.as_array() {
                for item in items {
                    walk(item, prefix, top, out, seen);
                }
            }
            return;
        };
        let kind = obj.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("");
        match kind {
            "Port" | "Variable" | "Net" if !name.is_empty() => {
                if let (Some(file), Some(line)) = (
                    obj.get("source_file").and_then(|f| f.as_str()),
                    obj.get("source_line").and_then(|l| l.as_u64()),
                ) {
                    let full = format!("{prefix}{name}");
                    if seen.insert(full.clone()) {
                        let file = file.rsplit('/').next().unwrap_or(file).to_string();
                        out.push((
                            full,
                            SourceLoc {
                                file,
                                line: u32::try_from(line).unwrap_or(u32::MAX),
                            },
                        ));
                    }
                }
            }
            "Instance" | "GenerateBlock" if !top && !name.is_empty() => {
                let child = format!("{prefix}{}.", segment(name));
                for key in ["body", "members"] {
                    if let Some(v) = obj.get(key) {
                        walk(v, &child, false, out, seen);
                    }
                }
                return;
            }
            _ => {}
        }
        // The top instance (a direct member of the root) keeps bare names; everything else
        // descends with the prefix it has.
        let next_top = top && kind != "Instance";
        for key in ["members", "body", "design"] {
            if let Some(v) = obj.get(key) {
                walk(v, prefix, next_top, out, seen);
            }
        }
    }
    walk(&root, "", true, &mut out, &mut seen);
    out
}

#[cfg(test)]
mod declaration_tests {
    use super::*;

    /// The AST shape slang dumps with `--ast-json-source-info` (trimmed from the pump-pair block
    /// with its bound checker, slang 12.1, 2026-10-29): the top instance's members bare, the bound
    /// instance's under its name, a port once although it appears twice, a generate block as `$`.
    #[test]
    fn declarations_follow_the_instance_tree_with_the_lifts_spelling() {
        let json = r#"{"design":{"kind":"Root","name":"$root","members":[
          {"kind":"CompilationUnit","members":[]},
          {"kind":"Instance","name":"pump_pair","source_line":18,"body":{"kind":"InstanceBody","name":"pump_pair","members":[
            {"kind":"Port","name":"state_q","source_file":"../rtl/pump_pair.sv","source_line":32},
            {"kind":"Variable","name":"state_q","source_file":"../rtl/pump_pair.sv","source_line":32},
            {"kind":"Variable","name":"lof_q","source_file":"../rtl/pump_pair.sv","source_line":36},
            {"kind":"ProceduralBlock","source_line":52},
            {"kind":"GenerateBlock","name":"gen[1]","members":[
              {"kind":"Variable","name":"q","source_file":"../rtl/pump_pair.sv","source_line":60}]},
            {"kind":"Instance","name":"u_pump_pair_sva","source_line":21,"body":{"kind":"InstanceBody","name":"pump_pair_sva","members":[
              {"kind":"Port","name":"state_q","source_file":"../rtl/pump_pair_sva.sv","source_line":16},
              {"kind":"Net","name":"state_q","source_file":"../rtl/pump_pair_sva.sv","source_line":16}]}}]}}]}}"#;
        let got: Vec<(String, String)> = declarations_from_ast(json)
            .into_iter()
            .map(|(n, l)| (n, l.to_string()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("state_q".into(), "pump_pair.sv:32".into()),
                ("lof_q".into(), "pump_pair.sv:36".into()),
                ("gen$1.q".into(), "pump_pair.sv:60".into()),
                (
                    "u_pump_pair_sva.state_q".into(),
                    "pump_pair_sva.sv:16".into()
                ),
            ]
        );
        assert!(declarations_from_ast("not json").is_empty());
    }
}
