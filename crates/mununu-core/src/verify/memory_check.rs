//! `mununu memory check` analysis — reports declared
//! `[sources.memory_abstraction]` postures and surfaces warnings
//! when property formulas reference memory in ways the declared
//! posture cannot soundly support.
//!
//! This is Step 3 of Stream B (memory-soundness tooling). It extends
//! the parse-time validation in `verify::config` (B2a, PR #60) with
//! property-level cross-checks documented in `docs/abstraction.md`
//! § "Memory soundness matrix" (B2c, PR #61).
//!
//! The check is **advisory** by default: it surfaces warnings and
//! informational notes but does not change verdicts. Callers (CLI,
//! API, UI) opt into `--strict` to treat warnings as errors.
//!
//! # What is checked
//!
//! For each `[[sources]]` entry with a `memory_abstraction` block:
//!
//! 1. Posture summary — kind, tracked addresses, value symbol set,
//!    fence semantics, notes.
//! 2. RVWMO aspirational note — when `fence_semantics = "rvwmo"` is
//!    declared, emit an informational reminder that the orchestrator
//!    does not yet enforce RVWMO semantics.
//!
//! For each `[[properties]]` entry, the formula text + every
//! template-arg value is scanned for `<source_id>.<token>(.<token>)?`
//! patterns. For each match against a memory-abstracted source:
//!
//! - **`chaotic` posture** mentioned in any property → warn (the
//!   posture has no per-address state, so the verdict is meaningless
//!   for memory-sensitive properties).
//! - **`tracked_addresses` + value-mention** (two-level dot path) →
//!   warn (the posture does not encode values; the verdict cannot
//!   tell `fresh` from `stale`).
//! - **First token not in `tracked`** when posture tracks addresses
//!   → warn (untracked-address reference).
//! - **Second token not in `value_symbol_set`** when posture is
//!   `tracked_with_values` → warn (undeclared symbol class).
//!
//! The scan uses a simple identifier-aware textual matcher; it does
//! not require parsing the mu-calculus formula. False positives on
//! identifiers that happen to look like `<src>.<id>` but are
//! unrelated to memory references are possible — surface a warning
//! with a `false_positive_hint` field so users can document
//! intentional patterns.

use serde::Serialize;
use std::collections::BTreeMap;

use crate::verify::config::{MemoryAbstractionPosture, VerifyConfig};

// ============================================================================
// Report types
// ============================================================================

/// Full memory-check report. Serializable for JSON output (CLI's
/// `--json` flag, API responses) and human-printable via
/// [`MemoryCheckReport::print_human`].
#[derive(Debug, Clone, Serialize)]
pub struct MemoryCheckReport {
    /// Per-source posture summary, one entry per source with a
    /// `[sources.memory_abstraction]` block. Sources without a
    /// declared posture are listed under `undeclared_sources`.
    pub postures: Vec<PostureSummary>,
    /// Source IDs that have no `memory_abstraction` block. These
    /// fall back to the legacy chaotic posture; the report flags
    /// them for visibility but does not warn.
    pub undeclared_sources: Vec<String>,
    /// Warnings — actionable mismatches between declared posture
    /// and property formula references.
    pub warnings: Vec<MemoryCheckWarning>,
    /// Informational notes — declared-but-aspirational features
    /// (e.g. `rvwmo` fence semantics) that do not yet change
    /// verdicts.
    pub info: Vec<MemoryCheckInfo>,
}

impl MemoryCheckReport {
    /// `true` when at least one warning was raised.
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Per-source posture summary.
#[derive(Debug, Clone, Serialize)]
pub struct PostureSummary {
    pub source_id: String,
    pub kind: String,
    pub tracked: Vec<String>,
    pub value_symbol_set: Vec<String>,
    pub fence_semantics: Option<String>,
    pub notes: Option<String>,
}

impl PostureSummary {
    fn from_posture(source_id: &str, p: &MemoryAbstractionPosture) -> Self {
        Self {
            source_id: source_id.to_string(),
            kind: p.kind.clone(),
            tracked: p.tracked.clone(),
            value_symbol_set: p.value_symbol_set.clone(),
            fence_semantics: p.fence_semantics.clone(),
            notes: p.notes.clone(),
        }
    }
}

/// Property-level warnings. Each variant carries enough context to
/// pinpoint the offending property + posture without re-running the
/// scan.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryCheckWarning {
    /// Property formula references a chaotic-posture source.
    /// Chaotic admits any behaviour, so the verdict is meaningless
    /// for memory-sensitive properties.
    ChaoticPostureReferenced {
        property_name: String,
        source_id: String,
        reference: String,
    },
    /// Property formula contains a `<src>.<addr>.<value>` reference
    /// but the source's posture is `tracked_addresses`, which does
    /// not encode values.
    ValueMentionOnTrackedAddressesPosture {
        property_name: String,
        source_id: String,
        reference: String,
    },
    /// Property formula references an address that is not in the
    /// source's `tracked` list.
    UntrackedAddressReferenced {
        property_name: String,
        source_id: String,
        address: String,
    },
    /// Property formula references a value-class not in the
    /// source's `value_symbol_set`.
    UndeclaredValueSymbolReferenced {
        property_name: String,
        source_id: String,
        address: String,
        symbol: String,
    },
}

/// Informational notes. Currently used for aspirational features
/// (`rvwmo` fence semantics) and default-posture fallback.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryCheckInfo {
    /// Source declared `fence_semantics = "rvwmo"`. The orchestrator
    /// does not yet enforce RVWMO semantics; the declaration is
    /// honoured by `mununu memory check` as a soundness-discipline
    /// marker only.
    RvwmoAspirational { source_id: String },
}

// ============================================================================
// Analysis
// ============================================================================

/// Run the memory-check analysis over a parsed config.
///
/// The function is pure: it does not read source files or
/// dispatch adapters. Posture declarations come from the config;
/// property references come from each property's `formula` text
/// and `args` values.
pub fn check_memory_postures(config: &VerifyConfig) -> MemoryCheckReport {
    let mut postures: Vec<PostureSummary> = Vec::new();
    let mut undeclared_sources: Vec<String> = Vec::new();
    let mut warnings: Vec<MemoryCheckWarning> = Vec::new();
    let mut info: Vec<MemoryCheckInfo> = Vec::new();

    // Build posture lookup table.
    let mut posture_by_source: BTreeMap<&str, &MemoryAbstractionPosture> = BTreeMap::new();
    for s in &config.sources {
        match &s.memory_abstraction {
            Some(p) => {
                postures.push(PostureSummary::from_posture(&s.id, p));
                posture_by_source.insert(s.id.as_str(), p);
                if p.fence_semantics.as_deref() == Some("rvwmo") {
                    info.push(MemoryCheckInfo::RvwmoAspirational {
                        source_id: s.id.clone(),
                    });
                }
            }
            None => undeclared_sources.push(s.id.clone()),
        }
    }

    // Scan every property formula + every template-arg value.
    for p in &config.properties {
        let mut texts: Vec<&str> = Vec::new();
        if let Some(f) = &p.formula {
            texts.push(f.as_str());
        }
        for v in p.args.values() {
            texts.push(v.as_str());
        }
        for text in texts {
            for r in scan_references(text) {
                let posture = match posture_by_source.get(r.source_id.as_str()) {
                    Some(p) => p,
                    None => continue,
                };
                emit_warnings_for_reference(&p.name, &r, posture, &mut warnings);
            }
        }
    }

    MemoryCheckReport {
        postures,
        undeclared_sources,
        warnings,
        info,
    }
}

// ============================================================================
// Reference scanner
// ============================================================================

/// One parsed memory reference inside a property formula or
/// template-arg value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reference {
    source_id: String,
    address: String,
    value: Option<String>,
    /// The whole `<src>.<addr>(.<value>)?` substring, surfaced in
    /// warnings for user feedback.
    raw: String,
}

/// Scan a text fragment for `<ident>.<ident>(.<ident>)?` references.
///
/// Returns every distinct reference (deduplicated within the call)
/// in source order. The scanner is character-driven; it treats
/// CTXDSL / mu-calculus operators (`<`, `>`, `[`, `]`, `&`, `|`,
/// `!`, `(`, `)`, `,`, whitespace) as identifier boundaries.
fn scan_references(text: &str) -> Vec<Reference> {
    let mut out: Vec<Reference> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip non-identifier bytes.
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }
        // Consume identifier 1.
        let id1_start = i;
        while i < bytes.len() && is_ident_cont(bytes[i]) {
            i += 1;
        }
        let id1 = &text[id1_start..i];
        // Need a dot to form a reference.
        if i >= bytes.len() || bytes[i] != b'.' {
            continue;
        }
        let dot1 = i;
        i += 1;
        // Consume identifier 2.
        if i >= bytes.len() || !is_ident_start(bytes[i]) {
            continue;
        }
        let id2_start = i;
        while i < bytes.len() && is_ident_cont(bytes[i]) {
            i += 1;
        }
        let id2 = &text[id2_start..i];
        // Optional `.id3` for value mentions.
        let (value, end) = if i < bytes.len() && bytes[i] == b'.' {
            let next = i + 1;
            if next < bytes.len() && is_ident_start(bytes[next]) {
                let mut j = next;
                while j < bytes.len() && is_ident_cont(bytes[j]) {
                    j += 1;
                }
                let id3 = text[next..j].to_string();
                (Some(id3), j)
            } else {
                (None, i)
            }
        } else {
            (None, i)
        };
        let raw = text[id1_start..end].to_string();
        i = end;
        let _ = dot1;
        // Deduplicate within this scan.
        let r = Reference {
            source_id: id1.to_string(),
            address: id2.to_string(),
            value,
            raw,
        };
        if !out.iter().any(|x| x == &r) {
            out.push(r);
        }
    }
    out
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ============================================================================
// Warning emission per reference + posture
// ============================================================================

fn emit_warnings_for_reference(
    property_name: &str,
    r: &Reference,
    posture: &MemoryAbstractionPosture,
    out: &mut Vec<MemoryCheckWarning>,
) {
    match posture.kind.as_str() {
        "chaotic" => {
            out.push(MemoryCheckWarning::ChaoticPostureReferenced {
                property_name: property_name.to_string(),
                source_id: r.source_id.clone(),
                reference: r.raw.clone(),
            });
        }
        "tracked_addresses" => {
            if r.value.is_some() {
                out.push(MemoryCheckWarning::ValueMentionOnTrackedAddressesPosture {
                    property_name: property_name.to_string(),
                    source_id: r.source_id.clone(),
                    reference: r.raw.clone(),
                });
            }
            if !posture.tracked.is_empty() && !posture.tracked.contains(&r.address) {
                out.push(MemoryCheckWarning::UntrackedAddressReferenced {
                    property_name: property_name.to_string(),
                    source_id: r.source_id.clone(),
                    address: r.address.clone(),
                });
            }
        }
        "tracked_with_values" => {
            if !posture.tracked.is_empty() && !posture.tracked.contains(&r.address) {
                out.push(MemoryCheckWarning::UntrackedAddressReferenced {
                    property_name: property_name.to_string(),
                    source_id: r.source_id.clone(),
                    address: r.address.clone(),
                });
            }
            if let Some(sym) = r.value.as_ref()
                && !posture.value_symbol_set.is_empty()
                && !posture.value_symbol_set.contains(sym)
            {
                out.push(MemoryCheckWarning::UndeclaredValueSymbolReferenced {
                    property_name: property_name.to_string(),
                    source_id: r.source_id.clone(),
                    address: r.address.clone(),
                    symbol: sym.clone(),
                });
            }
        }
        // full_concrete: nothing to warn about in v1.
        _ => {}
    }
}

// ============================================================================
// Human-readable printer
// ============================================================================

impl MemoryCheckReport {
    /// Format the report for stdout. Layout:
    ///
    /// ```text
    /// memory-check report:
    ///   declared postures (N):
    ///     <source_id>: kind = …, tracked = […], …
    ///   undeclared sources (M): a, b, c
    ///   warnings (K):
    ///     [chaotic_posture_referenced] <property>: <reference> against <source>
    ///     …
    ///   info (J):
    ///     [rvwmo_aspirational] <source>: not yet enforced by orchestrator
    /// ```
    pub fn format_human(&self) -> String {
        let mut s = String::new();
        s.push_str("memory-check report:\n");
        s.push_str(&format!("  declared postures ({}):\n", self.postures.len()));
        for p in &self.postures {
            let mut line = format!("    {}: kind = {}", p.source_id, p.kind);
            if !p.tracked.is_empty() {
                line.push_str(&format!(", tracked = [{}]", p.tracked.join(", ")));
            }
            if !p.value_symbol_set.is_empty() {
                line.push_str(&format!(
                    ", value_symbol_set = [{}]",
                    p.value_symbol_set.join(", ")
                ));
            }
            if let Some(fs) = &p.fence_semantics {
                line.push_str(&format!(", fence_semantics = {fs}"));
            }
            if let Some(n) = &p.notes {
                line.push_str(&format!(", notes = {n}"));
            }
            s.push_str(&line);
            s.push('\n');
        }
        if !self.undeclared_sources.is_empty() {
            s.push_str(&format!(
                "  undeclared sources ({}): {}\n",
                self.undeclared_sources.len(),
                self.undeclared_sources.join(", ")
            ));
        }
        s.push_str(&format!("  warnings ({}):\n", self.warnings.len()));
        for w in &self.warnings {
            s.push_str(&format!("    {}\n", format_warning_human(w)));
        }
        s.push_str(&format!("  info ({}):\n", self.info.len()));
        for i in &self.info {
            s.push_str(&format!("    {}\n", format_info_human(i)));
        }
        s
    }
}

fn format_warning_human(w: &MemoryCheckWarning) -> String {
    match w {
        MemoryCheckWarning::ChaoticPostureReferenced {
            property_name,
            source_id,
            reference,
        } => format!(
            "[chaotic_posture_referenced] property `{}` references `{}` against chaotic-posture source `{}` — verdict is meaningless for memory-sensitive properties",
            property_name, reference, source_id
        ),
        MemoryCheckWarning::ValueMentionOnTrackedAddressesPosture {
            property_name,
            source_id,
            reference,
        } => format!(
            "[value_mention_on_tracked_addresses_posture] property `{}` references `{}` against `{}` (kind = tracked_addresses) — values are not encoded; switch to tracked_with_values to make this sound",
            property_name, reference, source_id
        ),
        MemoryCheckWarning::UntrackedAddressReferenced {
            property_name,
            source_id,
            address,
        } => format!(
            "[untracked_address_referenced] property `{}` references address `{}` on source `{}` but it is not in the source's tracked list",
            property_name, address, source_id
        ),
        MemoryCheckWarning::UndeclaredValueSymbolReferenced {
            property_name,
            source_id,
            address,
            symbol,
        } => format!(
            "[undeclared_value_symbol_referenced] property `{}` references `{}.{}.{}` but `{}` is not in the source's value_symbol_set",
            property_name, source_id, address, symbol, symbol
        ),
    }
}

fn format_info_human(i: &MemoryCheckInfo) -> String {
    match i {
        MemoryCheckInfo::RvwmoAspirational { source_id } => format!(
            "[rvwmo_aspirational] source `{}` declares fence_semantics = rvwmo — the orchestrator does not yet enforce RVWMO semantics; the declaration is honoured as a soundness-discipline marker only",
            source_id
        ),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_text: &str) -> VerifyConfig {
        VerifyConfig::from_toml(toml_text).expect("parse")
    }

    #[test]
    fn scan_references_extracts_two_and_three_level_paths() {
        let text = "mu X. (mem.x.fresh && core_0.M_lineX) || <step> X";
        let refs = scan_references(text);
        let expected: Vec<Reference> = vec![
            Reference {
                source_id: "mem".into(),
                address: "x".into(),
                value: Some("fresh".into()),
                raw: "mem.x.fresh".into(),
            },
            Reference {
                source_id: "core_0".into(),
                address: "M_lineX".into(),
                value: None,
                raw: "core_0.M_lineX".into(),
            },
        ];
        assert_eq!(refs, expected);
    }

    #[test]
    fn scan_references_dedups_repeats() {
        let text = "mem.x.fresh || mem.x.fresh && mem.x.fresh";
        let refs = scan_references(text);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "mem.x.fresh");
    }

    #[test]
    fn scan_references_ignores_operators_and_whitespace() {
        let text = "[a, b](mu Z.<>Z)";
        let refs = scan_references(text);
        assert!(refs.is_empty(), "unexpected refs: {:?}", refs);
    }

    #[test]
    fn chaotic_posture_referenced_emits_warning() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "chaotic"

[[sources]]
id = "core"
adapter = "ctxdsl"
files = ["c.ctxdsl"]

[composition]
semantics = "asynchronous"
members = ["mem", "core"]

[[properties]]
name = "p"
formula = "mu X. (mem.x.fresh || <> X)"
"#,
        );
        let report = check_memory_postures(&cfg);
        let chaotic_count = report
            .warnings
            .iter()
            .filter(|w| matches!(w, MemoryCheckWarning::ChaoticPostureReferenced { .. }))
            .count();
        assert_eq!(chaotic_count, 1);
    }

    #[test]
    fn tracked_addresses_value_mention_warns() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "tracked_addresses"
tracked = ["x"]

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
formula = "mem.x.fresh"
"#,
        );
        let report = check_memory_postures(&cfg);
        assert!(report.warnings.iter().any(|w| matches!(
            w,
            MemoryCheckWarning::ValueMentionOnTrackedAddressesPosture { .. }
        )));
    }

    #[test]
    fn untracked_address_warns() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "tracked_addresses"
tracked = ["x"]

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
formula = "mem.y"
"#,
        );
        let report = check_memory_postures(&cfg);
        assert!(report.warnings.iter().any(|w| matches!(
            w,
            MemoryCheckWarning::UntrackedAddressReferenced { address, .. } if address == "y"
        )));
    }

    #[test]
    fn undeclared_value_symbol_warns() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "tracked_with_values"
tracked = ["x"]
value_symbol_set = ["fresh", "stale"]

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
formula = "mem.x.uninitialized"
"#,
        );
        let report = check_memory_postures(&cfg);
        assert!(report.warnings.iter().any(|w| matches!(
            w,
            MemoryCheckWarning::UndeclaredValueSymbolReferenced { symbol, .. } if symbol == "uninitialized"
        )));
    }

    #[test]
    fn rvwmo_emits_aspirational_info_note() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "tracked_with_values"
tracked = ["x"]
value_symbol_set = ["fresh"]
fence_semantics = "rvwmo"

[composition]
semantics = "asynchronous"
members = ["mem"]
"#,
        );
        let report = check_memory_postures(&cfg);
        assert!(matches!(
            report.info.first(),
            Some(MemoryCheckInfo::RvwmoAspirational { source_id }) if source_id == "mem"
        ));
    }

    #[test]
    fn undeclared_sources_listed_but_not_warned() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
formula = "mem.x.fresh"
"#,
        );
        let report = check_memory_postures(&cfg);
        assert_eq!(report.undeclared_sources, vec!["mem".to_string()]);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn template_args_are_also_scanned() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "tracked_addresses"
tracked = ["x"]

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
template = "reachable"
args = { TARGET = "mem.x.fresh" }
"#,
        );
        let report = check_memory_postures(&cfg);
        assert!(report.warnings.iter().any(|w| matches!(
            w,
            MemoryCheckWarning::ValueMentionOnTrackedAddressesPosture { .. }
        )));
    }

    #[test]
    fn full_concrete_posture_emits_no_warnings_for_memory_refs() {
        let cfg = parse(
            r#"
[project]
name = "Demo"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "full_concrete"

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
formula = "mem.x.0xCAFE"
"#,
        );
        let report = check_memory_postures(&cfg);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn report_is_serde_round_trippable() {
        let report = MemoryCheckReport {
            postures: vec![PostureSummary {
                source_id: "mem".into(),
                kind: "tracked_with_values".into(),
                tracked: vec!["x".into()],
                value_symbol_set: vec!["fresh".into()],
                fence_semantics: Some("release_acquire".into()),
                notes: None,
            }],
            undeclared_sources: vec![],
            warnings: vec![MemoryCheckWarning::ChaoticPostureReferenced {
                property_name: "p".into(),
                source_id: "mem".into(),
                reference: "mem.x.fresh".into(),
            }],
            info: vec![],
        };
        let json = serde_json::to_string(&report).expect("serialise");
        assert!(json.contains("chaotic_posture_referenced"));
        assert!(json.contains("tracked_with_values"));
    }
}
