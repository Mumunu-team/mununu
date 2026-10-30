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

/// What slang's own AST says about where the design's names live in the RTL (2026-10-29/30),
/// from one run with `--ast-json-source-info` on the ORIGINAL staged files (the SVA extraction's
/// own run works on a preprocessed copy whose lines are shifted). It does not depend on the lift's
/// front end: yosys-slang stamps lines on the netlist only in recent builds, yosys's `read_verilog`
/// does, sv2v never. Names are spelled as the lift spells them: the top instance's members bare, a
/// nested instance's as `inst.name`, a generate block's `gen[k]` as `gen$k`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceIndex {
    /// Every port, net and variable with its declaration; the first declaration of a name wins.
    pub declared: Vec<(String, crate::trace::SourceLoc)>,
    /// Per name: the header line of each edge-triggered procedural block (`always_ff`,
    /// `always @(posedge …)`) that assigns it — the line yosys stamps on the flop it infers.
    pub flop_blocks: std::collections::BTreeMap<String, Vec<crate::trace::SourceLoc>>,
    /// Per name: the line of every statement that writes it — procedural assignments in any
    /// `always` block and continuous `assign`s (not `initial` / `final`).
    pub writes: std::collections::BTreeMap<String, Vec<crate::trace::SourceLoc>>,
    /// Per concurrent assertion: its report name (the translator's), its label, its line.
    pub assertions: Vec<(String, Option<String>, crate::trace::SourceLoc)>,
}

/// Run slang once more with `--ast-json-source-info` and read the [`SourceIndex`] from its AST.
pub fn run_source_index(
    bin: &SlangBin,
    files: &[PathBuf],
    include_dirs: &[PathBuf],
    params: &[(String, String)],
) -> Result<SourceIndex, AdapterError> {
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
                "adapter/slang: the source-index run produced no AST (status {}). stderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            location: None,
        });
    }
    source_index_from_ast(&json).ok_or_else(|| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: "adapter/slang: the source-index run's AST is not valid JSON".to_string(),
        location: None,
    })
}

/// The [`SourceIndex`] of an AST dumped with `--ast-json-source-info`; `None` when it is not JSON.
/// Pure, so each part's shape is pinned by a test on real slang output.
pub fn source_index_from_ast(json: &str) -> Option<SourceIndex> {
    let root: serde_json::Value = {
        use serde::Deserialize as _;
        // Deeply nested expressions exceed serde_json's default recursion limit (the translator
        // lifts it for the same trusted AST).
        let mut de = serde_json::Deserializer::from_str(json);
        de.disable_recursion_limit();
        serde_json::Value::deserialize(&mut de).ok()?
    };
    let mut index = SourceIndex {
        assertions: translate::assertion_sites(&root),
        ..SourceIndex::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    let bodies = translate::collect_instance_bodies(&root);
    walk_scopes(&root, "", true, &bodies, &mut |obj, prefix| {
        let kind = obj.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("");
        match kind {
            "Port" | "Variable" | "Net" if !name.is_empty() => {
                if let Some(loc) = point_loc(obj, "source_file", "source_line") {
                    let full = format!("{prefix}{name}");
                    if seen.insert(full.clone()) {
                        index.declared.push((full, loc));
                    }
                }
            }
            "ProceduralBlock" => {
                let proc_kind = obj
                    .get("procedureKind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("");
                if matches!(proc_kind, "Initial" | "Final") {
                    return;
                }
                let Some(body) = obj.get("body") else { return };
                let mut targets: Vec<(String, crate::trace::SourceLoc)> = Vec::new();
                collect_assignments(body, &mut targets);
                let edge = proc_kind == "AlwaysFF" || (proc_kind == "Always" && has_edge(body));
                let block = point_loc(obj, "source_file", "source_line");
                for (target, at) in targets {
                    let full = format!("{prefix}{target}");
                    if edge && let Some(block) = &block {
                        push_unique(index.flop_blocks.entry(full.clone()).or_default(), block);
                    }
                    push_unique(index.writes.entry(full).or_default(), &at);
                }
            }
            "ContinuousAssign" => {
                if let Some(assignment) = obj.get("assignment") {
                    let mut targets = Vec::new();
                    collect_assignments(assignment, &mut targets);
                    for (target, at) in targets {
                        push_unique(
                            index.writes.entry(format!("{prefix}{target}")).or_default(),
                            &at,
                        );
                    }
                }
            }
            _ => {}
        }
    });
    // In line order: the walk visits a statement's keys alphabetically (`else` before `then`).
    for lines in index
        .flop_blocks
        .values_mut()
        .chain(index.writes.values_mut())
    {
        lines.sort();
    }
    Some(index)
}

/// The declarations of a slang AST dumped with `--ast-json-source-info` (the
/// [`SourceIndex::declared`] part on its own).
pub fn declarations_from_ast(json: &str) -> Vec<(String, crate::trace::SourceLoc)> {
    source_index_from_ast(json)
        .map(|i| i.declared)
        .unwrap_or_default()
}

/// A symbol's position (`source_file` / `source_line`) or a statement's (`source_file_start` /
/// `source_line_start`), as a basename and a line.
fn point_loc(
    obj: &serde_json::Map<String, serde_json::Value>,
    file_key: &str,
    line_key: &str,
) -> Option<crate::trace::SourceLoc> {
    let file = obj.get(file_key).and_then(|f| f.as_str())?;
    let line = obj.get(line_key).and_then(|l| l.as_u64())?;
    Some(crate::trace::SourceLoc {
        file: file.rsplit('/').next().unwrap_or(file).to_string(),
        line: u32::try_from(line).unwrap_or(u32::MAX),
    })
}

fn push_unique(list: &mut Vec<crate::trace::SourceLoc>, loc: &crate::trace::SourceLoc) {
    if !list.contains(loc) {
        list.push(loc.clone());
    }
}

/// Walk the instance tree, calling `visit` on every object with the name prefix it sits under:
/// `Root → members → Instance → body (InstanceBody) → members`. The top instance (a direct member
/// of the root) keeps bare names; a nested instance or a named generate block extends the prefix;
/// a `for` generate's iteration is `<array>$<genvar value>` (the lift's spelling of `gen[k]`,
/// mununu#713); an uninstantiated block (the branch elaboration did not take) is skipped; an
/// instance whose body slang serialised once and refers to by address is resolved through
/// `bodies` — the same rules the translator's assertion walk follows.
fn walk_scopes<'a>(
    node: &'a serde_json::Value,
    prefix: &str,
    top: bool,
    bodies: &std::collections::HashMap<u64, &'a serde_json::Value>,
    visit: &mut dyn FnMut(&'a serde_json::Map<String, serde_json::Value>, &str),
) {
    let Some(obj) = node.as_object() else {
        if let Some(items) = node.as_array() {
            for item in items {
                walk_scopes(item, prefix, top, bodies, visit);
            }
        }
        return;
    };
    // `u[3]` / `gen[3]` → `$3`, the lift's spelling (mununu#713).
    fn segment(name: &str) -> String {
        name.replace('[', "$").replace(']', "")
    }
    let kind = obj.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if obj.get("isUninstantiated").and_then(|u| u.as_bool()) == Some(true) {
        return;
    }
    match kind {
        "Instance" if !top && !name.is_empty() => {
            let child = format!("{prefix}{}.", segment(name));
            let body = match obj.get("body") {
                Some(serde_json::Value::String(r)) => r
                    .split(' ')
                    .next()
                    .and_then(|a| a.parse::<u64>().ok())
                    .and_then(|a| bodies.get(&a).copied()),
                other => other,
            };
            if let Some(body) = body {
                walk_scopes(body, &child, false, bodies, visit);
            }
            return;
        }
        "GenerateBlock" if !name.is_empty() => {
            let child = format!("{prefix}{}.", segment(name));
            if let Some(v) = obj.get("members") {
                walk_scopes(v, &child, false, bodies, visit);
            }
            return;
        }
        "GenerateBlockArray" => {
            let var = obj
                .get("loopVariable")
                .and_then(|v| v.as_str())
                .and_then(|v| v.rsplit(' ').next())
                .unwrap_or("");
            for block in obj
                .get("members")
                .and_then(|m| m.as_array())
                .into_iter()
                .flatten()
                .filter_map(|b| b.as_object())
                .filter(|b| b.get("kind").and_then(|k| k.as_str()) == Some("GenerateBlock"))
                .filter(|b| b.get("isUninstantiated").and_then(|u| u.as_bool()) != Some(true))
            {
                let genvar = block
                    .get("members")
                    .and_then(|m| m.as_array())
                    .into_iter()
                    .flatten()
                    .find(|m| {
                        m.get("kind").and_then(|k| k.as_str()) == Some("Parameter")
                            && m.get("name").and_then(|n| n.as_str()) == Some(var)
                    })
                    .and_then(|m| m.get("value"))
                    .and_then(|v| v.as_str())
                    .and_then(translate::parse_sv_literal);
                // An iteration whose index cannot be read keeps a prefix no lift name matches.
                let index = genvar.map_or_else(|| "?".to_string(), |v| v.to_string());
                let child = format!("{prefix}{name}${index}.");
                if let Some(v) = block.get("members") {
                    walk_scopes(v, &child, false, bodies, visit);
                }
            }
            return;
        }
        _ => {}
    }
    visit(obj, prefix);
    let next_top = top && kind != "Instance";
    for key in ["members", "body", "design"] {
        if let Some(v) = obj.get(key) {
            walk_scopes(v, prefix, next_top, bodies, visit);
        }
    }
}

/// Every `Assignment` under `node` (a procedural block's body, a continuous assign) with the
/// name(s) it writes and the statement's line. The written name is the left-hand side's root
/// signal: through a bit / part select and a member access, every operand of a concatenation;
/// never the select's index (which is read, not written).
fn collect_assignments(node: &serde_json::Value, out: &mut Vec<(String, crate::trace::SourceLoc)>) {
    fn targets(lhs: &serde_json::Value, out: &mut Vec<String>) {
        let Some(obj) = lhs.as_object() else { return };
        match obj.get("kind").and_then(|k| k.as_str()).unwrap_or("") {
            "NamedValue" => {
                if let Some(sym) = obj.get("symbol").and_then(|s| s.as_str()) {
                    // `"<addr> <name>"`.
                    let name = sym.split_once(' ').map_or(sym, |(_, n)| n);
                    out.push(name.to_string());
                }
            }
            "ElementSelect" | "RangeSelect" | "MemberAccess" => {
                if let Some(v) = obj.get("value") {
                    targets(v, out);
                }
            }
            "Concatenation" => {
                for op in obj
                    .get("operands")
                    .and_then(|o| o.as_array())
                    .into_iter()
                    .flatten()
                {
                    targets(op, out);
                }
            }
            _ => {}
        }
    }
    match node {
        serde_json::Value::Object(obj) => {
            if obj.get("kind").and_then(|k| k.as_str()) == Some("Assignment")
                && let Some(at) = point_loc(obj, "source_file_start", "source_line_start")
            {
                let mut names = Vec::new();
                if let Some(lhs) = obj.get("left") {
                    targets(lhs, &mut names);
                }
                for n in names {
                    out.push((n, at.clone()));
                }
            }
            for v in obj.values() {
                collect_assignments(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_assignments(v, out);
            }
        }
        _ => {}
    }
}

/// Whether a procedural block's timing control names a clock edge (`always @(posedge clk)`).
fn has_edge(body: &serde_json::Value) -> bool {
    let Some(timing) = body.get("timing") else {
        return false;
    };
    fn any_edge(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Object(o) => {
                matches!(
                    o.get("edge").and_then(|e| e.as_str()),
                    Some("PosEdge" | "NegEdge" | "BothEdges")
                ) || o.values().any(any_edge)
            }
            serde_json::Value::Array(a) => a.iter().any(any_edge),
            _ => false,
        }
    }
    any_edge(timing)
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

    const SOURCE_INDEX_FORMS: &str = include_str!("testdata/source_index_forms.ast.json");

    fn lines(
        index: &std::collections::BTreeMap<String, Vec<crate::trace::SourceLoc>>,
        name: &str,
    ) -> Vec<u32> {
        index
            .get(name)
            .map(|l| l.iter().map(|l| l.line).collect())
            .unwrap_or_default()
    }

    /// Real slang 11.0 output (`testdata/source_index_forms.sv`): an `always_comb` writes but is
    /// not a flop block; `always_ff` and Verilog `always @(posedge)` are; a bit-select writes its
    /// array (never the index it reads), a concatenation writes each operand, a continuous assign
    /// writes its target; a `for` generate's iterations are `gen$0` / `gen$1`, the second one's
    /// instance body serialised by address and still walked.
    #[test]
    fn the_source_index_reads_assignments_blocks_and_generate_iterations_from_real_slang() {
        let index = source_index_from_ast(SOURCE_INDEX_FORMS).expect("the fixture is JSON");
        assert_eq!(lines(&index.flop_blocks, "state_q"), vec![24]);
        assert_eq!(lines(&index.flop_blocks, "bits"), vec![29]);
        assert_eq!(lines(&index.flop_blocks, "a"), vec![29]);
        assert_eq!(lines(&index.flop_blocks, "b"), vec![29]);
        assert!(
            !index.flop_blocks.contains_key("state_d") && !index.flop_blocks.contains_key("done"),
            "combinational writes are not flop blocks"
        );
        assert!(!index.flop_blocks.contains_key("state_q[0]") && !index.writes.contains_key("go"));
        assert_eq!(lines(&index.writes, "state_d"), vec![20, 21]);
        assert_eq!(lines(&index.writes, "state_q"), vec![25, 26]);
        assert_eq!(lines(&index.writes, "bits"), vec![30]);
        assert_eq!(lines(&index.writes, "done"), vec![34]);
        for k in ["gen$0.u_leaf.q", "gen$1.u_leaf.q"] {
            assert_eq!(lines(&index.flop_blocks, k), vec![5], "{k}");
        }
        let declared = |name: &str| {
            index
                .declared
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, l)| l.to_string())
        };
        assert_eq!(
            declared("state_q").as_deref(),
            Some("source_index_forms.sv:12")
        );
        assert_eq!(
            declared("state_d").as_deref(),
            Some("source_index_forms.sv:14")
        );
        assert_eq!(
            declared("gen$1.u_leaf.q").as_deref(),
            Some("source_index_forms.sv:4")
        );
        let sites: Vec<(String, Option<String>, u32)> = index
            .assertions
            .iter()
            .map(|(n, l, at)| (n.clone(), l.clone(), at.line))
            .collect();
        assert_eq!(
            sites,
            vec![
                ("top_sva_0".to_string(), Some("a_wraps".to_string()), 40),
                ("top_sva_1".to_string(), None, 41),
            ]
        );
    }

    /// The seam the property's `source` line rests on: the index names each assertion exactly as
    /// the translator names its row, label included, on the same AST.
    #[test]
    fn the_index_names_each_assertion_as_the_translator_names_its_row() {
        let report =
            translate::translate_ast_json(SOURCE_INDEX_FORMS).expect("the fixture translates");
        assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
        let mut rows: Vec<(String, Option<String>)> = report
            .translated
            .iter()
            .map(|t| (t.name.clone(), t.label.clone()))
            .collect();
        rows.sort();
        let index = source_index_from_ast(SOURCE_INDEX_FORMS).expect("JSON");
        let mut sites: Vec<(String, Option<String>)> = index
            .assertions
            .iter()
            .map(|(n, l, _)| (n.clone(), l.clone()))
            .collect();
        sites.sort();
        assert_eq!(rows, sites);
    }
}
