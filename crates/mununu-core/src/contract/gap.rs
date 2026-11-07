//! Gap markers — explicit "unknown" regions in a contract.
//!
//! Task A3 of `docs/archive/design/black-box-modules.md`. When the discovery
//! pipeline (task A5/A6) encounters a black-box module it cannot fully
//! characterise, it emits one or more `GapMarker` records describing
//! *what* is unknown and *why* it matters. These are then:
//!
//! 1. Surfaced as structured `tracing::warn!` diagnostics so the user sees
//!    every gap rather than mununu silently falling back to a chaotic
//!    stub.
//! 2. Serialised into a `.contract.todo.json` skeleton so the user can
//!    fill them in without re-deriving the structure.
//! 3. Treated as hard errors under `--strict-contracts` (CI / safety-
//!    critical mode).
//!
//! A3 ships the *infrastructure*; A5 is the producer that actually
//! generates `GapMarker` records from real adapters.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// What is missing about a black-box module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// Output sequencing of the module is unknown — outputs are modelled
    /// as nondeterministic. Sound for safety, unsound for liveness.
    OutputSequencing,
    /// Latency / bounded-response behaviour is unknown.
    LatencyBound,
    /// Per-input protocol assumption is unstated (e.g., "input is held
    /// stable before strobe rises").
    InputAssumption,
    /// State predicate over interface signals is unstated (e.g., "full
    /// and empty are never both high").
    StatePredicate,
    /// Fairness / liveness assumption is unstated.
    Fairness,
    /// Other / unclassified gap — populated with a free-form description.
    Other,
}

impl GapKind {
    /// Human-readable explanation of the soundness consequence of leaving
    /// this gap as a chaotic stub. Used in the diagnostic message so the
    /// user knows what they are accepting.
    pub fn soundness_note(self) -> &'static str {
        match self {
            GapKind::OutputSequencing => {
                "safety verdicts hold; liveness verdicts depending on \
                 these labels are unsound (no progress assumption)."
            }
            GapKind::LatencyBound => {
                "bounded-time properties cannot be discharged without an \
                 authored latency bound."
            }
            GapKind::InputAssumption => {
                "controller may be over-cautious; verdicts about the \
                 surrounding logic remain sound for safety."
            }
            GapKind::StatePredicate => {
                "properties that rely on the unstated invariant cannot \
                 be discharged."
            }
            GapKind::Fairness => {
                "liveness verdicts unsound under chaotic environment; \
                 safety verdicts unaffected."
            }
            GapKind::Other => "see description for soundness implications.",
        }
    }
}

impl fmt::Display for GapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            GapKind::OutputSequencing => "output_sequencing",
            GapKind::LatencyBound => "latency_bound",
            GapKind::InputAssumption => "input_assumption",
            GapKind::StatePredicate => "state_predicate",
            GapKind::Fairness => "fairness",
            GapKind::Other => "other",
        };
        write!(f, "{name}")
    }
}

/// A single declared gap in the discovered contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapMarker {
    /// Name of the module / component the gap belongs to.
    pub module: String,
    /// Kind of gap.
    pub kind: GapKind,
    /// The interface labels / ports / fields the gap touches (e.g.
    /// `["full", "empty"]` for an `OutputSequencing` gap on those
    /// signals).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Free-form description for `Other` or to clarify the gap context.
    #[serde(default)]
    pub description: Option<String>,
    /// Source location, if known (filename + line). Survives into the
    /// diagnostic so the user can navigate to the unparsed instantiation.
    #[serde(default)]
    pub source_location: Option<SourceLocation>,
}

/// File + line provenance for a gap marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
}

/// A collected report of gap markers, plus a flag indicating strict mode.
///
/// Produced by the discovery pipeline (A5), consumed by:
/// - the diagnostic emitter (A3, `emit_diagnostics`),
/// - the `.contract.todo.json` emitter (A3, `to_todo_json`),
/// - the `--strict-contracts` exit-code gate (A3, `is_strict_failure`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapMarkerReport {
    /// Gaps discovered during extraction, in deterministic order.
    pub markers: Vec<GapMarker>,
}

impl GapMarkerReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, marker: GapMarker) {
        self.markers.push(marker);
    }

    pub fn extend(&mut self, markers: impl IntoIterator<Item = GapMarker>) {
        self.markers.extend(markers);
    }

    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.markers.len()
    }

    /// Group markers by module so per-module reports / sidecars can be
    /// written. Preserves intra-module order.
    pub fn by_module(&self) -> std::collections::BTreeMap<&str, Vec<&GapMarker>> {
        let mut grouped: std::collections::BTreeMap<&str, Vec<&GapMarker>> =
            std::collections::BTreeMap::new();
        for marker in &self.markers {
            grouped
                .entry(marker.module.as_str())
                .or_default()
                .push(marker);
        }
        grouped
    }

    /// Whether this report should fail an extraction running under
    /// `--strict-contracts`. Currently: any gap marker is a failure;
    /// a future refinement could allow per-kind allow-lists.
    pub fn is_strict_failure(&self) -> bool {
        !self.markers.is_empty()
    }

    /// Emit one structured `tracing::warn!` per gap marker. Designed for
    /// human-readable logs; the structured fields (`module`, `kind`,
    /// `labels`) let downstream tools filter and aggregate.
    pub fn emit_diagnostics(&self) {
        for marker in &self.markers {
            let labels_joined = marker.labels.join(", ");
            let loc = marker
                .source_location
                .as_ref()
                .map(|s| format!("{}:{}", s.file, s.line))
                .unwrap_or_else(|| "<unknown>".to_string());
            tracing::warn!(
                module = marker.module.as_str(),
                kind = %marker.kind,
                labels = labels_joined.as_str(),
                location = loc.as_str(),
                soundness = marker.kind.soundness_note(),
                description = marker.description.as_deref().unwrap_or(""),
                "contract gap detected — chaotic stub default in effect"
            );
        }
    }

    /// Serialise to the `.contract.todo.json` shape: gap markers grouped
    /// by module, pre-filled with empty slots the user must complete.
    pub fn to_todo_json(&self) -> String {
        let payload = serde_json::json!({
            "_format": "contract.todo.v1",
            "_note": "Auto-generated gap report. Fill in `proposed_*` fields and rerun.",
            "gaps": self.markers.iter().map(|m| {
                serde_json::json!({
                    "module": m.module,
                    "kind": m.kind.to_string(),
                    "labels": m.labels,
                    "description": m.description,
                    "source_location": m.source_location,
                    "soundness_note": m.kind.soundness_note(),
                    "proposed_assumption": null,
                    "proposed_guarantee": null,
                })
            }).collect::<Vec<_>>(),
        });
        serde_json::to_string_pretty(&payload)
            .expect("GapMarkerReport always serialises to valid JSON")
    }

    /// Convenience: write the `.contract.todo.json` sidecar next to a
    /// source file. The target path is `<source>.contract.todo.json`.
    pub fn write_todo_sidecar(&self, source: &Path) -> std::io::Result<std::path::PathBuf> {
        let mut target = source.to_path_buf();
        let original_filename = target
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "contract".to_string());
        target.set_file_name(format!("{original_filename}.contract.todo.json"));
        std::fs::write(&target, self.to_todo_json())?;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(module: &str, kind: GapKind, labels: &[&str]) -> GapMarker {
        GapMarker {
            module: module.to_string(),
            kind,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            description: None,
            source_location: None,
        }
    }

    #[test]
    fn empty_report_is_not_strict_failure() {
        let report = GapMarkerReport::new();
        assert!(report.is_empty());
        assert!(!report.is_strict_failure());
    }

    #[test]
    fn report_with_marker_fails_strict_mode() {
        let mut report = GapMarkerReport::new();
        report.push(marker(
            "FifoIp",
            GapKind::OutputSequencing,
            &["full", "empty"],
        ));
        assert!(report.is_strict_failure());
    }

    #[test]
    fn gap_kind_soundness_notes_are_distinct() {
        let notes: std::collections::HashSet<&'static str> = [
            GapKind::OutputSequencing,
            GapKind::LatencyBound,
            GapKind::InputAssumption,
            GapKind::StatePredicate,
            GapKind::Fairness,
            GapKind::Other,
        ]
        .iter()
        .map(|k| k.soundness_note())
        .collect();
        // Every kind has its own non-empty soundness note.
        assert_eq!(notes.len(), 6);
        assert!(notes.iter().all(|n| !n.is_empty()));
    }

    #[test]
    fn by_module_groups_markers() {
        let mut report = GapMarkerReport::new();
        report.push(marker("SHA", GapKind::OutputSequencing, &["hash_out"]));
        report.push(marker("RSA", GapKind::LatencyBound, &["verify_done"]));
        report.push(marker("SHA", GapKind::Fairness, &[]));
        let grouped = report.by_module();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped["SHA"].len(), 2);
        assert_eq!(grouped["RSA"].len(), 1);
    }

    #[test]
    fn to_todo_json_round_trips() {
        let mut report = GapMarkerReport::new();
        report.push(GapMarker {
            module: "FifoIp".to_string(),
            kind: GapKind::OutputSequencing,
            labels: vec!["full".to_string(), "empty".to_string()],
            description: Some("Vendor wrapper has no sequencing annotation.".to_string()),
            source_location: Some(SourceLocation {
                file: "rtl/fifo.sv".to_string(),
                line: 42,
            }),
        });
        let json = report.to_todo_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["_format"], "contract.todo.v1");
        let gaps = parsed["gaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0]["module"], "FifoIp");
        assert_eq!(gaps[0]["kind"], "output_sequencing");
        assert_eq!(gaps[0]["labels"][0], "full");
        assert!(gaps[0]["proposed_assumption"].is_null());
        assert!(gaps[0]["proposed_guarantee"].is_null());
    }

    #[test]
    fn write_todo_sidecar_creates_file_next_to_source() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("design.sv");
        fs::write(&source, "// dummy").unwrap();
        let mut report = GapMarkerReport::new();
        report.push(marker("FifoIp", GapKind::OutputSequencing, &["full"]));
        let target = report.write_todo_sidecar(&source).unwrap();
        assert_eq!(
            target.file_name().unwrap().to_string_lossy(),
            "design.sv.contract.todo.json"
        );
        let body = fs::read_to_string(&target).unwrap();
        assert!(body.contains("FifoIp"));
        assert!(body.contains("output_sequencing"));
    }
}
