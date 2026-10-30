//! XL.6a — SVA extraction orchestration (SV source → translated property set).
//!
//! Ties the three XL.1/XL.3 pieces — [`locate_slang`], [`run_ast_json`](crate::adapter::slang::run_ast_json), and
//! [`translate_ast_json`] — into one string-in / [`TranslationReport`]-out
//! helper, so the SVA front-end is reachable from a user-facing surface (the
//! `sv/extract-sva` CLI subcommand + HTTP endpoint + UI panel).
//!
//! In-memory SV sources are staged into one temp dir, which is also slang's
//! include path so a `` `include `` of a sibling source resolves (e.g. real
//! OpenTitan RTL `` `include "prim_assert.sv" `` when the standard macros are
//! passed alongside — the dummy-macro variant silently drops all SVA, the XL.0
//! gotcha). The temp dir is removed when the call returns.

use std::path::PathBuf;

use crate::adapter::slang::translate::{
    TranslateOptions, TranslationReport, translate_ast_json_with_source,
};
use crate::adapter::slang::{locate_slang, run_ast_json_with_source};
use crate::adapter::{AdapterError, AdapterErrorKind};

/// Extract + translate every concurrent SVA assertion from a set of in-memory
/// SV sources. `sources` is `(file_name, content)` pairs; the first is the
/// primary. Returns the [`TranslationReport`] (translated formulas + their
/// recoverability companions, honestly-recorded unsupported assertions, and the
/// `__past` shadow registers the translated formulas need).
///
/// `Err` if no sources are given, slang is not installed (a structured
/// `UnsupportedConstruct` error — the feature degrades, it never panics), or
/// slang emits no AST.
pub fn extract_sva(sources: &[(String, String)]) -> Result<TranslationReport, AdapterError> {
    extract_sva_with_options(sources, &TranslateOptions::default())
}

/// Like [`extract_sva`] but honoring [`TranslateOptions`] — e.g. `gate_reset`
/// to drop `disable iff` guards so the verify-auto consumer can pin the reset
/// input inactive at the model level.
pub fn extract_sva_with_options(
    sources: &[(String, String)],
    opts: &TranslateOptions,
) -> Result<TranslationReport, AdapterError> {
    extract_sva_with_params(sources, opts, &[])
}

/// Like [`extract_sva_with_options`] but elaborating the design under `--param` overrides
/// (`NAME` or `MODULE.NAME`, value) — mununu#717. The verify-auto path must call this with the
/// same overrides the lift applies, or the SVA describes the design at its parameter defaults.
pub fn extract_sva_with_params(
    sources: &[(String, String)],
    opts: &TranslateOptions,
    params: &[(String, String)],
) -> Result<TranslationReport, AdapterError> {
    extract_sva_with_declarations(sources, opts, params).map(|(report, _)| report)
}

/// Where the design's names and assertions live in the RTL ([`super::SourceIndex`]: declarations,
/// the `always` blocks and statements that write each name, each assertion's line), or why that
/// slang run failed (2026-10-29/30).
pub type Declarations = Result<super::SourceIndex, String>;

/// [`extract_sva_with_params`] plus the [`super::SourceIndex`] of the same staged files, from a
/// second slang run ([`super::run_source_index`]) — the front-end-independent half of the source
/// map (2026-10-29/30). That run's failure is returned as a reason, never as the extraction's
/// failure: the verdicts do not depend on it, the legend says what it lacks.
pub fn extract_sva_with_declarations(
    sources: &[(String, String)],
    opts: &TranslateOptions,
    params: &[(String, String)],
) -> Result<(TranslationReport, Declarations), AdapterError> {
    if sources.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: "adapter/slang/extract: no SV sources provided".to_string(),
            location: None,
        });
    }

    // Probe slang first, so a missing binary errors cleanly before we stage.
    let bin = locate_slang()?;

    let dir = ScopedTempDir::new()?;

    let mut files: Vec<PathBuf> = Vec::with_capacity(sources.len());
    for (name, content) in sources {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AdapterError {
                kind: AdapterErrorKind::ParseError,
                message: format!("adapter/slang/extract: could not stage `{name}`: {e}"),
                location: None,
            })?;
        }
        std::fs::write(&path, content).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/slang/extract: could not write `{name}`: {e}"),
            location: None,
        })?;
        files.push(path);
    }

    // The staging dir doubles as the include search path.
    let include_dirs = vec![dir.path().to_path_buf()];
    // mununu#722 — the preprocessed text rides along, so a hierarchical reference into a module
    // instantiated more than once can be read from the source it was written in.
    let (json, source) = run_ast_json_with_source(&bin, &files, &include_dirs, params)?;
    let report = translate_ast_json_with_source(&json, Some(&source), opts)?;
    let declarations =
        super::run_source_index(&bin, &files, &include_dirs, params).map_err(|e| e.message);
    Ok((report, declarations))
    // `dir` drops here → temp files removed.
}

/// A per-call temp dir with Drop cleanup (`tempfile` is a dev-only dep, so the
/// library hand-rolls this, mirroring `adapter::yosys`'s private `TempDir`).
struct ScopedTempDir {
    path: PathBuf,
}

impl ScopedTempDir {
    fn new() -> Result<Self, AdapterError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "mununu-slang-sva-{}-{}-{}",
            std::process::id(),
            nanos,
            seq
        ));
        std::fs::create_dir_all(&path).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/slang/extract: could not create temp dir: {e}"),
            location: None,
        })?;
        Ok(ScopedTempDir { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sources_is_an_error() {
        let err = extract_sva(&[]).expect_err("no sources must error");
        assert_eq!(err.kind, AdapterErrorKind::ParseError);
        assert!(err.message.contains("no SV sources"));
    }

    #[test]
    #[ignore = "requires the slang CLI (MUNUNU_SLANG_PATH or $PATH); run with --ignored"]
    fn extracts_inline_sva_end_to_end() {
        let sv = (
            "tiny.sv".to_string(),
            "module tiny (input logic clk, input logic a, input logic b);\n\
             ap: assert property (@(posedge clk) a |-> b);\n\
             cp: cover property (@(posedge clk) a && b);\n\
             endmodule\n"
                .to_string(),
        );
        let report = extract_sva(&[sv]).expect("extract");
        assert_eq!(report.total(), 2, "one assert + one cover");
        assert_eq!(report.unsupported.len(), 0, "both are Tier-1");
        // The cover carries its recoverability companion (XL.2).
        assert!(
            report
                .translated
                .iter()
                .any(|t| t.recoverability_companion.is_some()),
            "the cover should carry an AG-EF recoverability companion"
        );
    }
}
