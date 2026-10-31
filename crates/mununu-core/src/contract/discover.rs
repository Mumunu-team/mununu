//! Discovery pipeline phase 1 — task A5 of
//! `docs/design/black-box-modules.md`.
//!
//! Phase 1 covers the first three fields of the contract object the
//! pipeline emits per black-box module:
//!
//! 1. **Interface alphabet** — the labels derived from the module's port
//!    list (or function signature in the software analogue).
//! 2. **Controllability classification** per label, computed via the
//!    shared `crate::controllability` helper (task A4).
//! 3. **Gap markers** — explicit `unknown { … }` regions describing what
//!    is *not* known about the module, so the user sees the soundness
//!    consequence of accepting the default chaotic stub.
//!
//! Phase 2 (task A6) adds discovered automaton fragments from
//! source-comment annotations and corpus lookups; phase 3 adds discovered
//! formulas. Both are out of scope for this module.
//!
//! The phase-1 deliverable is intentionally adapter-agnostic. A
//! `BlackBoxInterface` describes the bare minimum the adapter must hand
//! over (module name + ordered list of `PortDescriptor`s); the rest of
//! the pipeline is shared. Adapters supply this description today by
//! synthesising it from whatever language-specific representation they
//! have — SV port lists, MCP tool schemas, C function signatures, …

use crate::clts::LabelControllability;
use crate::contract::contract_uri::{ContractUri, parse_contract_uri};
use crate::contract::gap::{GapKind, GapMarker, GapMarkerReport};
use crate::controllability::{BoundaryDirection, classify_label};
use crate::corpus::Corpus;
use serde::{Deserialize, Serialize};

/// A single port / parameter / channel on a black-box module's interface.
///
/// The adapter-supplied description; `discover_phase1` translates this
/// into the contract-side `InterfaceLabel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortDescriptor {
    /// Label name as it will appear in the CLTS alphabet.
    pub name: String,
    /// Direction relative to the module's boundary.
    pub direction: BoundaryDirection,
    /// Optional human-readable description (e.g. signal width, channel
    /// semantics, MCP tool docstring). Carried into the gap-marker
    /// metadata so the user sees the original intent.
    #[serde(default)]
    pub description: Option<String>,
}

/// What the adapter knows about a black-box module before phase 1 runs.
///
/// This is the smallest abstraction that lets the same discovery
/// pipeline serve SV, BTOR2, and software extraction. Each adapter is
/// responsible for filling this in from its own native representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlackBoxInterface {
    /// Module / component / class name.
    pub name: String,
    /// Ports in declaration order. The pipeline preserves order so the
    /// emitted alphabet is deterministic.
    pub ports: Vec<PortDescriptor>,
    /// Optional source location (file + line) for diagnostics.
    #[serde(default)]
    pub source_file: Option<String>,
    /// Optional source line for diagnostics.
    #[serde(default)]
    pub source_line: Option<u32>,
    /// Source-comment annotations (`@mununu_*` tags) attached to this
    /// module. Populated by Document D task D4 — both the yosys
    /// frontend (from yosys's `write_json` attribute map) and the
    /// custom-SV frontend (from `extract_from_sv_source`) feed the same
    /// `MununuAnnotation` shape here. Phase-2 discovery (task A6)
    /// uses these to:
    ///   - replace the default `OutputSequencing` gap with a smaller
    ///     gap when `@mununu_guarantee` clauses are present;
    ///   - flag `@mununu_interface` URIs for corpus lookup;
    ///   - apply `@mununu_controllable` / `@mununu_uncontrollable`
    ///     overrides before the §4 classifier runs.
    #[serde(default)]
    pub annotations: Vec<crate::mununu_annotations::MununuAnnotation>,
}

/// A single classified interface label emitted by phase 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceLabel {
    pub name: String,
    /// Controllability after applying the shared classifier + any
    /// adapter-supplied overrides.
    pub controllability: LabelControllability,
    /// Original direction reported by the adapter.
    pub direction: BoundaryDirection,
    pub description: Option<String>,
}

/// Output of phase 1 discovery for one black-box module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase1Output {
    /// Module name, copied through.
    pub module: String,
    /// Classified interface labels.
    pub labels: Vec<InterfaceLabel>,
    /// Gap markers attached to this module (always emitted in phase 1 —
    /// the discovery pipeline has not yet learned anything about
    /// sequencing or formulas, so every black-box module starts with at
    /// least an `OutputSequencing` gap).
    pub gaps: GapMarkerReport,
    /// Corpus lookups produced from `@mununu_interface contract://`
    /// annotations during phase 2. Empty when no `contract://` URI is
    /// referenced or when no corpus was supplied to `DiscoverOptions`.
    #[serde(default)]
    pub corpus_resolutions: Vec<CorpusResolution>,
}

/// Outcome of resolving a single `@mununu_interface contract://` URI
/// against the supplied corpus.
///
/// Document D §D.2's query helper is purely functional — this struct
/// surfaces the result back to the user so HITL UX can show:
/// "vendor declared `contract://X@Y`; mununu found 2 candidates in
/// the corpus; chose Z by parameter-match + provenance."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusResolution {
    /// The URI as it appeared on the annotation.
    pub raw_uri: String,
    /// Parsed shape of the URI.
    pub parsed: ContractUri,
    /// What happened — found / not-found / no-corpus / malformed.
    pub status: ResolutionStatus,
    /// IDs of corpus entries that matched (top-ranked first). Empty for
    /// any non-`Resolved` status.
    #[serde(default)]
    pub matched_ids: Vec<String>,
    /// If the URI requested an alternative, whether that alternative
    /// exists on the chosen entry. `None` when no alternative was
    /// requested.
    #[serde(default)]
    pub alternative_matched: Option<bool>,
}

/// Outcome of a single corpus lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    /// Found at least one matching entry; `matched_ids` carries them.
    Resolved,
    /// The URI parsed but no corpus entry matched the
    /// `(domain, name)` tuple.
    NotFound,
    /// No corpus was passed via `DiscoverOptions::corpus`.
    NoCorpus,
    /// The URI failed to parse — kept here so the diagnostic surfaces
    /// the malformed annotation rather than silently dropping it.
    Malformed,
    /// The URI used an unrecognised scheme (e.g. a relative sidecar
    /// path). The annotation is preserved for HITL UX but no corpus
    /// lookup was attempted.
    SidecarReference,
}

/// Configuration knobs for `discover_phase1`.
#[derive(Debug, Clone, Default)]
pub struct DiscoverOptions<'a> {
    /// Per-label overrides — names listed here are forced to
    /// `Controllable` regardless of port direction. Used as the legacy
    /// escape hatch per Document A §4.ii.
    pub force_controllable: &'a [&'a str],
    /// Per-label overrides — names forced to `Uncontrollable`.
    pub force_uncontrollable: &'a [&'a str],
    /// Whether to emit a default `Fairness` gap marker as well as the
    /// usual `OutputSequencing` one. Adapters that know the module is
    /// purely combinational (no liveness story) can disable this.
    pub emit_fairness_gap: bool,
    /// Optional corpus for resolving `@mununu_interface contract://`
    /// annotations. When supplied, phase-2 discovery issues a corpus
    /// query per URI and records the outcome in
    /// `Phase1Output::corpus_resolutions`. When `None`, URI annotations
    /// are recorded with `ResolutionStatus::NoCorpus` so the
    /// diagnostic still surfaces the unresolved reference.
    pub corpus: Option<&'a Corpus>,
}

/// Convert a list of discovered black-box interfaces into the adapter
/// sidecar files the rest of the contract subsystem consumes.
///
/// Used by adapters that have just detected one or more black-box
/// submodules during extraction (e.g., yosys frontend hitting
/// `(* blackbox *)`, or the custom-SV pipeline encountering an
/// instantiation not listed in its multi-module sidecar). The
/// adapter calls this helper to get a `Vec<AdapterSidecar>` it can
/// attach to its `AdapterOutput`; the caller (CLI / API) writes the
/// sidecars to disk next to the primary CTXDSL output.
///
/// For each black-box interface, two sidecars are emitted:
///   - `<module>.interface.json` — the `BlackBoxInterface` itself.
///   - `<module>.gap_report.json` — a `GapMarkerReport` with the
///     phase-1 default gaps (OutputSequencing covering outputs;
///     Fairness opt-in).
///
/// This is Document B § B.7.3's load-bearing helper: the moment the
/// contract subsystem stops being a separate JSON workflow and becomes
/// an automatic byproduct of extraction.
pub fn build_blackbox_sidecars(
    interfaces: &[BlackBoxInterface],
    options: &DiscoverOptions<'_>,
) -> Vec<crate::adapter::AdapterSidecar> {
    use crate::adapter::{AdapterSidecar, SidecarOrigin};

    let mut sidecars = Vec::with_capacity(interfaces.len() * 2);
    for iface in interfaces {
        let phase1 = discover_phase1(iface, options);

        // Interface JSON. Use `to_string_pretty` so the file is
        // human-readable and stable for diffing.
        if let Ok(interface_json) = serde_json::to_string_pretty(&iface) {
            sidecars.push(AdapterSidecar {
                filename: format!("{}.interface.json", sanitize_filename(&iface.name)),
                content: interface_json,
                origin: SidecarOrigin::BlackBoxInterface,
            });
        }

        // Gap-report JSON.
        if let Ok(gap_json) = serde_json::to_string_pretty(&phase1.gaps) {
            sidecars.push(AdapterSidecar {
                filename: format!("{}.gap_report.json", sanitize_filename(&iface.name)),
                content: gap_json,
                origin: SidecarOrigin::BlackBoxGapReport,
            });
        }
    }
    sidecars
}

/// Make a filename-safe version of a module name. Lowercase ASCII +
/// digits + `_` + `-` are kept; everything else becomes `_`.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A2.6-style summary of the annotations a `BlackBoxInterface`
/// carries — used by `discover_phase1` to decide what kind of gap to
/// emit and by future HITL UX to surface the contract-source mix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationSummary {
    /// Whether `@mununu_blackbox` is present (redundant once the
    /// caller has decided it's a black box, but kept so downstream
    /// reports can show "user-marked" vs "adapter-inferred").
    pub has_blackbox_tag: bool,
    /// Number of `@mununu_assume` clauses.
    pub assume_count: usize,
    /// Number of `@mununu_guarantee` clauses.
    pub guarantee_count: usize,
    /// Contract URIs referenced via `@mununu_interface <uri>`.
    /// Populated as raw strings; downstream code parses
    /// `contract://domain/name@version[?alt=…]` into a corpus query.
    pub interface_refs: Vec<String>,
    /// Per-label controllability overrides.
    pub controllable_overrides: Vec<String>,
    /// Per-label uncontrollability overrides.
    pub uncontrollable_overrides: Vec<String>,
}

impl AnnotationSummary {
    /// Build a summary by walking the annotations of a black-box
    /// interface.
    pub fn from_annotations(annotations: &[crate::mununu_annotations::MununuAnnotation]) -> Self {
        use crate::mununu_annotations::MununuTag;
        let mut s = AnnotationSummary::default();
        for ann in annotations {
            match ann.tag {
                MununuTag::Blackbox => s.has_blackbox_tag = true,
                MununuTag::Assume => s.assume_count += 1,
                MununuTag::Guarantee => s.guarantee_count += 1,
                MununuTag::Interface => {
                    if !ann.value.is_empty() {
                        s.interface_refs.push(ann.value.clone());
                    }
                }
                MununuTag::Controllable => {
                    if !ann.value.is_empty() {
                        s.controllable_overrides.push(ann.value.clone());
                    }
                }
                MununuTag::Uncontrollable => {
                    if !ann.value.is_empty() {
                        s.uncontrollable_overrides.push(ann.value.clone());
                    }
                }
                // Phase L6: `@mununu_isr` does not affect the
                // discovery summary — it's consumed by the
                // codesign extractor's ISR detection, not by the
                // contract pipeline. Ignore here.
                MununuTag::Isr => {}
                // `@mununu_predicate` is a cube-abstraction hint consumed by
                // `sv verify-auto`'s seeder, not by the contract pipeline. Ignore.
                MununuTag::Predicate => {}
                // `@mununu_template` (2026-10-30) is a guarantee written as a template
                // instance; for discovery it is one more progress clause the author asserted.
                MununuTag::Template => s.guarantee_count += 1,
            }
        }
        s
    }

    /// Whether at least one *progress* clause is present
    /// (`@mununu_guarantee`). When true, the output-sequencing gap can
    /// be downgraded to a latency-bound gap because the user has
    /// asserted at least some sequencing behaviour about the outputs.
    pub fn has_progress_clause(&self) -> bool {
        self.guarantee_count > 0
    }
}

/// Run phase 1 discovery on a black-box interface. Always emits at least
/// one gap marker (chaotic-stub default).
///
/// **Phase-2 behaviour (Document A task A6).** When the interface carries
/// `@mununu_guarantee` annotations, the default `OutputSequencing` gap
/// is downgraded to a `LatencyBound` gap — the user has authored at
/// least some sequencing behaviour about the outputs, so the verifier
/// no longer needs to assume *no* progress. The gap's description
/// includes the count of A/G clauses so the HITL review can see what
/// the contract actually contains.
pub fn discover_phase1(iface: &BlackBoxInterface, options: &DiscoverOptions<'_>) -> Phase1Output {
    // 0. Summarise any source-comment annotations attached to the
    //    interface. The summary drives the §A6 phase-2 adjustments:
    //    annotation-derived overrides are folded into the
    //    controllability classifier, and the default
    //    `OutputSequencing` gap is downgraded to a `LatencyBound`
    //    gap when at least one guarantee clause is present.
    let summary = AnnotationSummary::from_annotations(&iface.annotations);
    let extra_controllable: Vec<&str> = summary
        .controllable_overrides
        .iter()
        .map(String::as_str)
        .collect();
    let extra_uncontrollable: Vec<&str> = summary
        .uncontrollable_overrides
        .iter()
        .map(String::as_str)
        .collect();

    // 1. Classify each port via the shared controllability helper.
    //    Annotation-derived overrides are merged with whatever the
    //    caller passed; both lists win over port direction.
    let labels: Vec<InterfaceLabel> = iface
        .ports
        .iter()
        .map(|port| {
            // Build a per-port view of the merged override lists. We
            // collect into owned vecs because the call signature
            // takes `&[&str]` slices.
            let mut force_c: Vec<&str> = options.force_controllable.to_vec();
            force_c.extend(extra_controllable.iter().copied());
            let mut force_u: Vec<&str> = options.force_uncontrollable.to_vec();
            force_u.extend(extra_uncontrollable.iter().copied());
            let controllability = classify_label(&port.name, port.direction, &force_c, &force_u);
            InterfaceLabel {
                name: port.name.clone(),
                controllability,
                direction: port.direction,
                description: port.description.clone(),
            }
        })
        .collect();

    // 2. Gap markers — at minimum, an `OutputSequencing` gap covering all
    //    outputs (the chaotic-stub default cannot prove liveness on
    //    output labels). Phase 2 (this branch, when at least one
    //    `@mununu_guarantee` annotation is present) downgrades it to a
    //    `LatencyBound` gap — the user has authored at least some
    //    sequencing behaviour about the outputs, so the verifier no
    //    longer needs to assume *no* progress.
    let mut gaps = GapMarkerReport::new();
    let output_labels: Vec<String> = labels
        .iter()
        .filter(|l| matches!(l.direction, BoundaryDirection::Output))
        .map(|l| l.name.clone())
        .collect();
    let source_location = match (iface.source_file.as_ref(), iface.source_line) {
        (Some(file), Some(line)) => Some(crate::contract::gap::SourceLocation {
            file: file.clone(),
            line,
        }),
        _ => None,
    };
    // 2.b Resolve any `@mununu_interface contract://` URIs against the
    //     corpus (when supplied). A successful resolution is treated as
    //     evidence of progress — equivalent in effect to one or more
    //     guarantee clauses, so the gap is downgraded the same way.
    let corpus_resolutions = resolve_interface_uris(&summary.interface_refs, options.corpus);
    let corpus_hit = corpus_resolutions
        .iter()
        .any(|r| matches!(r.status, ResolutionStatus::Resolved));

    if !output_labels.is_empty() {
        let (kind, description) = if summary.has_progress_clause() || corpus_hit {
            let suffix = if corpus_hit {
                format!(
                    " + {} corpus resolution(s)",
                    corpus_resolutions
                        .iter()
                        .filter(|r| matches!(r.status, ResolutionStatus::Resolved))
                        .count()
                )
            } else {
                String::new()
            };
            (
                GapKind::LatencyBound,
                format!(
                    "Phase-2 discovery — {} guarantee clause(s){} found on {}; \
                     latency bound still unauthored",
                    summary.guarantee_count, suffix, iface.name
                ),
            )
        } else {
            (
                GapKind::OutputSequencing,
                format!(
                    "Phase-1 discovery — no sequencing fragment yet for {}",
                    iface.name
                ),
            )
        };
        gaps.push(GapMarker {
            module: iface.name.clone(),
            kind,
            labels: output_labels,
            description: Some(description),
            source_location: source_location.clone(),
        });
    }
    if options.emit_fairness_gap {
        gaps.push(GapMarker {
            module: iface.name.clone(),
            kind: GapKind::Fairness,
            labels: vec![],
            description: Some("Phase-1 discovery — no fairness assumption authored.".to_string()),
            source_location,
        });
    }

    Phase1Output {
        module: iface.name.clone(),
        labels,
        gaps,
        corpus_resolutions,
    }
}

/// Resolve a list of `@mununu_interface` URI strings against the
/// supplied corpus. Every URI produces exactly one
/// `CorpusResolution` so the diagnostic surfaces what was found *and*
/// what was missing.
fn resolve_interface_uris(
    interface_refs: &[String],
    corpus: Option<&Corpus>,
) -> Vec<CorpusResolution> {
    use std::collections::BTreeMap;
    let mut out = Vec::with_capacity(interface_refs.len());
    for raw in interface_refs {
        // Distinguish `contract://` URIs from opaque sidecar paths so
        // HITL UX can surface the difference.
        let parsed = match parse_contract_uri(raw) {
            Some(u) => u,
            None => {
                if raw
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("contract://")
                {
                    out.push(CorpusResolution {
                        raw_uri: raw.clone(),
                        parsed: ContractUri {
                            domain: String::new(),
                            name: String::new(),
                            version: None,
                            alternative: None,
                            raw: raw.clone(),
                        },
                        status: ResolutionStatus::Malformed,
                        matched_ids: Vec::new(),
                        alternative_matched: None,
                    });
                } else {
                    out.push(CorpusResolution {
                        raw_uri: raw.clone(),
                        parsed: ContractUri {
                            domain: String::new(),
                            name: String::new(),
                            version: None,
                            alternative: None,
                            raw: raw.clone(),
                        },
                        status: ResolutionStatus::SidecarReference,
                        matched_ids: Vec::new(),
                        alternative_matched: None,
                    });
                }
                continue;
            }
        };
        let Some(corp) = corpus else {
            out.push(CorpusResolution {
                raw_uri: raw.clone(),
                parsed,
                status: ResolutionStatus::NoCorpus,
                matched_ids: Vec::new(),
                alternative_matched: None,
            });
            continue;
        };
        // Phase-2 corpus query: empty parameter map. The annotation
        // grammar does not carry parameters yet; a richer URI shape
        // (`?param=value`) is reserved for follow-up work.
        let params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let mut hits = corp.query(&parsed.domain, &parsed.name, &params);
        // If a version was pinned, narrow the candidates first; if
        // nothing matches the pin, keep the broader ranked list so the
        // HITL UX can show "you asked for @2.0.1 but the corpus only
        // has @1.0.0 / @2.0.0".
        if let Some(version) = &parsed.version {
            let pinned: Vec<_> = hits
                .iter()
                .copied()
                .filter(|e| e.version == *version)
                .collect();
            if !pinned.is_empty() {
                hits = pinned;
            }
        }
        if hits.is_empty() {
            out.push(CorpusResolution {
                raw_uri: raw.clone(),
                parsed,
                status: ResolutionStatus::NotFound,
                matched_ids: Vec::new(),
                alternative_matched: None,
            });
            continue;
        }
        let matched_ids: Vec<String> = hits
            .iter()
            .map(|e| format!("{}@{}", e.id, e.version))
            .collect();
        let alternative_matched = parsed.alternative.as_ref().map(|alt| {
            hits.first()
                .is_some_and(|e| e.alternatives.iter().any(|a| a.id == *alt))
        });
        out.push(CorpusResolution {
            raw_uri: raw.clone(),
            parsed,
            status: ResolutionStatus::Resolved,
            matched_ids,
            alternative_matched,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Alternative, ContractEntry, Provenance};
    use crate::mununu_annotations::{MununuAnnotation, MununuTag};

    fn port(name: &str, direction: BoundaryDirection) -> PortDescriptor {
        PortDescriptor {
            name: name.to_string(),
            direction,
            description: None,
        }
    }

    fn annotation(tag: MununuTag, value: &str) -> MununuAnnotation {
        MununuAnnotation::new(tag, value)
    }

    fn corpus_with_axi() -> crate::corpus::Corpus {
        crate::corpus::Corpus::from_entries(vec![ContractEntry {
            id: "rtl_protocol/axi4_slave".to_string(),
            version: "2.0.1".to_string(),
            domain: "rtl_protocol".to_string(),
            name: "axi4_slave".to_string(),
            description: None,
            parameters: Default::default(),
            contract: None,
            alternatives: vec![Alternative {
                id: "strict".to_string(),
                label: "Strict".to_string(),
                description: None,
            }],
            provenance: Provenance::MununuVerified {
                verified_against: None,
            },
            soundness_flag: None,
        }])
    }

    #[test]
    fn ports_classified_via_shared_rule() {
        let iface = BlackBoxInterface {
            name: "FifoIp".to_string(),
            ports: vec![
                port("push", BoundaryDirection::Input),
                port("pop", BoundaryDirection::Input),
                port("data_out", BoundaryDirection::Output),
                port("full", BoundaryDirection::Output),
                port("clk", BoundaryDirection::Input),
            ],
            source_file: None,
            source_line: None,
            annotations: Vec::new(),
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert_eq!(out.module, "FifoIp");
        assert_eq!(out.labels.len(), 5);
        let by_name = |name: &str| {
            out.labels
                .iter()
                .find(|l| l.name == name)
                .expect("label exists")
        };
        assert_eq!(
            by_name("push").controllability,
            LabelControllability::Uncontrollable
        );
        assert_eq!(
            by_name("data_out").controllability,
            LabelControllability::Controllable
        );
        assert_eq!(
            by_name("full").controllability,
            LabelControllability::Controllable
        );
    }

    #[test]
    fn output_sequencing_gap_emitted_when_outputs_exist() {
        let iface = BlackBoxInterface {
            name: "SHA256".to_string(),
            ports: vec![
                port("din", BoundaryDirection::Input),
                port("hash_out", BoundaryDirection::Output),
                port("hash_valid", BoundaryDirection::Output),
            ],
            source_file: Some("rtl/sha.sv".to_string()),
            source_line: Some(42),
            annotations: Vec::new(),
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert_eq!(out.gaps.len(), 1);
        let gap = &out.gaps.markers[0];
        assert_eq!(gap.module, "SHA256");
        assert_eq!(gap.kind, GapKind::OutputSequencing);
        assert_eq!(gap.labels.len(), 2);
        assert_eq!(gap.source_location.as_ref().map(|l| l.line), Some(42));
    }

    #[test]
    fn no_output_no_default_gap() {
        // Pure-input "sink" component — no outputs → no
        // OutputSequencing gap.
        let iface = BlackBoxInterface {
            name: "Sink".to_string(),
            ports: vec![
                port("din", BoundaryDirection::Input),
                port("strobe", BoundaryDirection::Input),
            ],
            source_file: None,
            source_line: None,
            annotations: Vec::new(),
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert!(out.gaps.is_empty());
    }

    #[test]
    fn fairness_gap_when_opt_in() {
        let iface = BlackBoxInterface {
            name: "Arbiter".to_string(),
            ports: vec![
                port("req", BoundaryDirection::Input),
                port("grant", BoundaryDirection::Output),
            ],
            source_file: None,
            source_line: None,
            annotations: Vec::new(),
        };
        let opts = DiscoverOptions {
            emit_fairness_gap: true,
            ..DiscoverOptions::default()
        };
        let out = discover_phase1(&iface, &opts);
        assert_eq!(out.gaps.len(), 2);
        assert_eq!(out.gaps.markers[0].kind, GapKind::OutputSequencing);
        assert_eq!(out.gaps.markers[1].kind, GapKind::Fairness);
    }

    #[test]
    fn build_sidecars_produces_interface_and_gap_files() {
        use crate::adapter::SidecarOrigin;

        let iface = BlackBoxInterface {
            name: "DDR3_PHY_V2".to_string(),
            ports: vec![
                port("clk", BoundaryDirection::Input),
                port("data_out", BoundaryDirection::Output),
            ],
            source_file: Some("rtl/vendor/ddr3.sv".to_string()),
            source_line: Some(10),
            annotations: Vec::new(),
        };
        let sidecars = build_blackbox_sidecars(&[iface], &DiscoverOptions::default());
        assert_eq!(sidecars.len(), 2);
        assert_eq!(sidecars[0].origin, SidecarOrigin::BlackBoxInterface);
        assert_eq!(sidecars[0].filename, "DDR3_PHY_V2.interface.json");
        assert!(sidecars[0].content.contains("\"name\": \"DDR3_PHY_V2\""));
        assert_eq!(sidecars[1].origin, SidecarOrigin::BlackBoxGapReport);
        assert_eq!(sidecars[1].filename, "DDR3_PHY_V2.gap_report.json");
        // Phase-1 emits an OutputSequencing gap for any black box with
        // at least one output.
        assert!(sidecars[1].content.contains("output_sequencing"));
        assert!(sidecars[1].content.contains("data_out"));
    }

    #[test]
    fn build_sidecars_sanitises_filename_chars() {
        let iface = BlackBoxInterface {
            name: "vendor/IP::Block 1".to_string(),
            ports: vec![port("out", BoundaryDirection::Output)],
            source_file: None,
            source_line: None,
            annotations: Vec::new(),
        };
        let sidecars = build_blackbox_sidecars(&[iface], &DiscoverOptions::default());
        assert_eq!(sidecars[0].filename, "vendor_IP__Block_1.interface.json");
    }

    #[test]
    fn annotations_summary_counts_each_tag_kind() {
        use crate::mununu_annotations::{MununuAnnotation, MununuTag};
        let anns = vec![
            MununuAnnotation::new(MununuTag::Blackbox, ""),
            MununuAnnotation::new(MununuTag::Assume, "G(reset -> idle within 8)"),
            MununuAnnotation::new(MununuTag::Guarantee, "G(req -> ack)"),
            MununuAnnotation::new(MununuTag::Guarantee, "G(write -> response within K)"),
            MununuAnnotation::new(MununuTag::Interface, "contract://rtl_memory/axi4_slave@2"),
            MununuAnnotation::new(MununuTag::Controllable, "reset_n"),
        ];
        let summary = AnnotationSummary::from_annotations(&anns);
        assert!(summary.has_blackbox_tag);
        assert_eq!(summary.assume_count, 1);
        assert_eq!(summary.guarantee_count, 2);
        assert_eq!(
            summary.interface_refs,
            vec!["contract://rtl_memory/axi4_slave@2".to_string()]
        );
        assert_eq!(summary.controllable_overrides, vec!["reset_n".to_string()]);
        assert!(summary.has_progress_clause());
    }

    #[test]
    fn phase1_downgrades_gap_when_guarantee_present() {
        use crate::mununu_annotations::{MununuAnnotation, MununuTag};
        let iface = BlackBoxInterface {
            name: "DDR_PHY".to_string(),
            ports: vec![
                port("clk", BoundaryDirection::Input),
                port("data_out", BoundaryDirection::Output),
            ],
            source_file: Some("rtl/ddr.sv".to_string()),
            source_line: Some(10),
            annotations: vec![MununuAnnotation::new(
                MununuTag::Guarantee,
                "G(awvalid -> awready)",
            )],
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert_eq!(out.gaps.len(), 1);
        assert_eq!(
            out.gaps.markers[0].kind,
            GapKind::LatencyBound,
            "presence of @mununu_guarantee should downgrade OutputSequencing to LatencyBound"
        );
        assert!(
            out.gaps.markers[0]
                .description
                .as_deref()
                .unwrap_or("")
                .contains("Phase-2"),
            "description should signal phase-2 vs phase-1"
        );
    }

    #[test]
    fn phase1_keeps_output_sequencing_when_no_guarantees() {
        let iface = BlackBoxInterface {
            name: "PlainBlackBox".to_string(),
            ports: vec![port("out", BoundaryDirection::Output)],
            source_file: None,
            source_line: None,
            annotations: vec![],
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert_eq!(out.gaps.markers[0].kind, GapKind::OutputSequencing);
    }

    #[test]
    fn annotation_overrides_steer_controllability() {
        use crate::mununu_annotations::{MununuAnnotation, MununuTag};
        // `reset_n` is an Input → would normally classify as
        // Uncontrollable. The `@mununu_controllable` annotation flips
        // it. `data_out` is an Output → normally Controllable; the
        // `@mununu_uncontrollable` annotation flips it.
        let iface = BlackBoxInterface {
            name: "Quirky".to_string(),
            ports: vec![
                port("reset_n", BoundaryDirection::Input),
                port("data_out", BoundaryDirection::Output),
            ],
            source_file: None,
            source_line: None,
            annotations: vec![
                MununuAnnotation::new(MununuTag::Controllable, "reset_n"),
                MununuAnnotation::new(MununuTag::Uncontrollable, "data_out"),
            ],
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        let by = |name: &str| out.labels.iter().find(|l| l.name == name).unwrap();
        assert_eq!(
            by("reset_n").controllability,
            LabelControllability::Controllable
        );
        assert_eq!(
            by("data_out").controllability,
            LabelControllability::Uncontrollable
        );
    }

    #[test]
    fn build_sidecars_empty_input_yields_empty_output() {
        let sidecars = build_blackbox_sidecars(&[], &DiscoverOptions::default());
        assert!(sidecars.is_empty());
    }

    #[test]
    fn force_lists_override_direction() {
        let iface = BlackBoxInterface {
            name: "Quirky".to_string(),
            ports: vec![
                port("reset_n", BoundaryDirection::Input),
                port("status", BoundaryDirection::Output),
            ],
            source_file: None,
            source_line: None,
            annotations: Vec::new(),
        };
        let opts = DiscoverOptions {
            force_controllable: &["reset_n"],
            force_uncontrollable: &["status"],
            ..DiscoverOptions::default()
        };
        let out = discover_phase1(&iface, &opts);
        let by = |name: &str| {
            out.labels
                .iter()
                .find(|l| l.name == name)
                .expect("label exists")
        };
        assert_eq!(
            by("reset_n").controllability,
            LabelControllability::Controllable
        );
        assert_eq!(
            by("status").controllability,
            LabelControllability::Uncontrollable
        );
    }

    #[test]
    fn corpus_uri_resolves_against_supplied_corpus() {
        let corpus = corpus_with_axi();
        let iface = BlackBoxInterface {
            name: "vendor_axi".to_string(),
            ports: vec![
                port("clk", BoundaryDirection::Input),
                port("status", BoundaryDirection::Output),
            ],
            source_file: None,
            source_line: None,
            annotations: vec![annotation(
                MununuTag::Interface,
                "contract://rtl_protocol/axi4_slave@2.0.1?alt=strict",
            )],
        };
        let opts = DiscoverOptions {
            corpus: Some(&corpus),
            ..DiscoverOptions::default()
        };
        let out = discover_phase1(&iface, &opts);
        assert_eq!(out.corpus_resolutions.len(), 1);
        let res = &out.corpus_resolutions[0];
        assert!(matches!(res.status, ResolutionStatus::Resolved));
        assert_eq!(res.matched_ids, vec!["rtl_protocol/axi4_slave@2.0.1"]);
        assert_eq!(res.alternative_matched, Some(true));
        // A corpus hit downgrades the OutputSequencing gap to LatencyBound,
        // and the description names the corpus resolution.
        assert_eq!(out.gaps.markers[0].kind, GapKind::LatencyBound);
        assert!(
            out.gaps.markers[0]
                .description
                .as_ref()
                .unwrap()
                .contains("corpus resolution")
        );
    }

    #[test]
    fn corpus_uri_with_no_corpus_records_no_corpus_status() {
        let iface = BlackBoxInterface {
            name: "vendor_axi".to_string(),
            ports: vec![port("status", BoundaryDirection::Output)],
            source_file: None,
            source_line: None,
            annotations: vec![annotation(
                MununuTag::Interface,
                "contract://rtl_protocol/axi4_slave",
            )],
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert_eq!(out.corpus_resolutions.len(), 1);
        assert!(matches!(
            out.corpus_resolutions[0].status,
            ResolutionStatus::NoCorpus
        ));
        // No corpus hit and no guarantee clause → gap stays OutputSequencing.
        assert_eq!(out.gaps.markers[0].kind, GapKind::OutputSequencing);
    }

    #[test]
    fn corpus_uri_not_found_records_not_found_status() {
        let corpus = corpus_with_axi();
        let iface = BlackBoxInterface {
            name: "vendor_unknown".to_string(),
            ports: vec![port("status", BoundaryDirection::Output)],
            source_file: None,
            source_line: None,
            annotations: vec![annotation(
                MununuTag::Interface,
                "contract://rtl_memory/ddr3@1.0",
            )],
        };
        let opts = DiscoverOptions {
            corpus: Some(&corpus),
            ..DiscoverOptions::default()
        };
        let out = discover_phase1(&iface, &opts);
        assert_eq!(out.corpus_resolutions.len(), 1);
        assert!(matches!(
            out.corpus_resolutions[0].status,
            ResolutionStatus::NotFound
        ));
        // No hit → gap stays OutputSequencing.
        assert_eq!(out.gaps.markers[0].kind, GapKind::OutputSequencing);
    }

    #[test]
    fn non_contract_uri_is_treated_as_sidecar_reference() {
        let iface = BlackBoxInterface {
            name: "vendor".to_string(),
            ports: vec![port("o", BoundaryDirection::Output)],
            source_file: None,
            source_line: None,
            annotations: vec![annotation(MununuTag::Interface, "./local-sidecar.json")],
        };
        let out = discover_phase1(&iface, &DiscoverOptions::default());
        assert_eq!(out.corpus_resolutions.len(), 1);
        assert!(matches!(
            out.corpus_resolutions[0].status,
            ResolutionStatus::SidecarReference
        ));
    }
}
