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

use std::path::PathBuf;
use std::process::Command;

use crate::adapter::{AdapterError, AdapterErrorKind};

pub mod expectations;
pub mod extract;
pub mod prim_stubs;
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
    let mut cmd = Command::new(&bin.path);
    // `--quiet` suppresses slang's "Top level design units" / build-summary
    // banner so stdout is *pure* JSON (the `-` sink). Without it the banner
    // prefixes the JSON and downstream `serde_json` parsing fails at column 1.
    cmd.arg("--ast-json")
        .arg("-")
        .arg("--quiet")
        .arg("--single-unit");
    for d in include_dirs {
        cmd.arg("-I").arg(d);
    }
    for f in files {
        cmd.arg(f);
    }

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
