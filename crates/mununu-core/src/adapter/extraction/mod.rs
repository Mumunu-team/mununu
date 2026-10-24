//! Extraction spec adapter.
//!
//! Translates extraction spec JSON (`.espec.json`) into CTXDSL via the
//! explicit-automaton encoding path. Extraction specs are produced by the
//! extraction pipeline:
//!
//! ```text
//! Source code → human extraction → JSON spec → this adapter → CTXDSL
//! ```
//!
//! The spec's `model_config` section carries declarative automaton definitions
//! with mode-filtered transitions. Each transition may be tagged with an
//! arbitrary `mode` string (e.g., `"fixed"`, `"vulnerable"`, `"as_audited"`,
//! `"with_provider_cache"`); the special value `"both"` (the default) means
//! "always include." The `--mode` CLI flag selects which transitions to
//! include: a transition is kept iff its `mode` is `"both"` or matches the
//! requested mode. The universal defaults `"fixed"`, `"vulnerable"`, and
//! `"both"` are always accepted by the CLI; any other mode value is accepted
//! iff at least one transition in the loaded spec is tagged with it.

pub mod ast;
pub mod ast_extract;
pub mod dep_graph;
pub mod validate;

use super::ir::*;
use super::{
    AdapterError, AdapterErrorKind, AdapterOptions, AdapterOutput, AdapterWarning, FormatAdapter,
    SourceFormat, SourceInfo, WarningKind,
};
use ast::{AutomatonDef, ExtractionSpec};

/// Extraction spec adapter implementing [`FormatAdapter`].
pub struct ExtractionAdapter;

impl FormatAdapter for ExtractionAdapter {
    fn detect(content: &str) -> bool {
        let trimmed = content.trim_start();
        if !trimmed.starts_with('{') {
            return false;
        }
        // Extraction specs have "$schema": "extraction_spec_v1" and "model_config"
        trimmed.contains("\"extraction_spec_v1\"") && trimmed.contains("\"model_config\"")
    }

    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
        let spec: ExtractionSpec = serde_json::from_str(content).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("Extraction spec JSON parse error: {e}"),
            location: None,
        })?;

        let mode = options.mode.as_deref().unwrap_or("vulnerable");

        // Build the set of valid modes: the universal defaults plus the union
        // of per-transition `mode` tags found in the loaded spec. This lets
        // spec authors use any investigation framing (e.g., `as_audited` vs
        // `with_provider_cache`) without being locked into the bug/fix
        // dichotomy.
        let mut valid_modes: std::collections::HashSet<&str> =
            ["fixed", "vulnerable", "both"].into_iter().collect();
        for aut in &spec.model_config.automata {
            for t in &aut.transitions {
                valid_modes.insert(t.mode.as_str());
            }
        }

        if !valid_modes.contains(mode) {
            let mut sorted: Vec<&str> = valid_modes.iter().copied().collect();
            sorted.sort();
            return Err(AdapterError {
                kind: AdapterErrorKind::ParseError,
                message: format!(
                    "Invalid mode '{mode}'. Valid modes for this spec: {}.",
                    sorted.join(", ")
                ),
                location: None,
            });
        }

        let mut warnings = Vec::new();

        // Phase A.3 step 3.4 — extraction-side auto-partition preview.
        //
        // The trait impl in `dep_graph` exposes the spec's
        // `state_fields[]` as the per-field signal set and uses
        // `methods[].guards/effects` references as dep edges.
        // For specs that have at least one declared field, run the
        // partition and surface any `Dropped` field as a preview
        // warning. Specs that use only `model_config.automata[]`
        // (no `state_fields`) have nothing per-field to prune and
        // the partition is a silent no-op.
        //
        // SOUNDNESS: see `dep_graph` module-level docs for the
        // indirect-reference caveat — the dep edges built from
        // `guards` / `effects` may under-represent the true data
        // flow when the upstream `mununu-extract` tool could not
        // resolve a pointer write to a concrete field name. A
        // standing AdapterWarning surfaces this risk whenever the
        // spec declares any fields.
        if !spec.state_fields.is_empty() {
            use crate::adapter::partition::{self, PartitionClass, PartitionOptions};
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "auto-partition (preview, step 3.4): extraction adapter's dep graph is derived from `methods[].guards/effects`; \
                     indirect pointer writes (`*p = x` and equivalents) may be missing — see phase-a3-followup-indirect-references.md. \
                     Spec declares {} state field(s).",
                    spec.state_fields.len()
                ),
                location: None,
            });
            let seeds = dep_graph::extract_property_seeds(&spec);
            let preview = partition::classify(&spec, &seeds, &PartitionOptions::default());
            for (name, class) in &preview.classes {
                if let PartitionClass::Dropped { reason } = class {
                    warnings.push(AdapterWarning {
                        kind: WarningKind::ApproximateTranslation,
                        message: format!(
                            "auto-partition (preview, step 3.4): extraction field '{name}' would be dropped as {reason} — \
                             step 3.5 will make this take effect on the sidecar resolver"
                        ),
                        location: None,
                    });
                }
            }
        }

        let ir = to_ir(&spec, mode, options, &mut warnings)?;

        let result = super::emit::emit(&ir).map_err(|e| AdapterError {
            kind: AdapterErrorKind::EmitError,
            message: format!("CTXDSL emission failed: {e}"),
            location: None,
        })?;

        let property_count = ir.properties.len();
        let signal_count = count_labels(&spec);

        // Prepend provenance headers for traceability
        let ctxdsl = build_provenance_header(&spec, mode) + &result.ctxdsl;

        Ok(AdapterOutput {
            sidecars: Vec::new(),
            ctxdsl,
            warnings,
            source_info: SourceInfo {
                format: SourceFormat::Extraction,
                title: Some(spec.model_config.context_name.clone()),
                signal_count,
                state_count: result.state_count,
                property_count,
            },
            state_valuations: Default::default(),
            transition_observations: Default::default(),
            partition_summary: None,
        })
    }
}

/// Convert an extraction spec to the shared AdapterIR.
fn to_ir(
    spec: &ExtractionSpec,
    mode: &str,
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<AdapterIR, AdapterError> {
    let config = &spec.model_config;

    let context_name = options
        .context_name
        .as_deref()
        .unwrap_or(&config.context_name);

    // Determine effective context name (append _vulnerable/_fixed suffix if appropriate)
    let effective_name = context_name.to_string();

    // Build automata from declarative definitions
    let automata: Vec<AutomatonSpec> = config
        .automata
        .iter()
        .map(|def| build_automaton(def, mode, warnings))
        .collect::<Result<Vec<_>, _>>()?;

    if automata.is_empty() {
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: "No automata defined in model_config; CTXDSL will have no automata"
                .to_string(),
            location: None,
        });
    }

    // Build composition
    let compositions = match &config.composition {
        Some(comp) => {
            let comp_spec = match comp.type_.as_str() {
                "synchronous" => CompositionSpec::Synchronous {
                    name: comp.name.clone(),
                    members: comp.members.clone(),
                },
                _ => CompositionSpec::Asynchronous {
                    name: comp.name.clone(),
                    members: comp.members.clone(),
                },
            };
            vec![comp_spec]
        }
        None => vec![],
    };

    // Build properties (raw formula takes precedence; template_ref used as fallback).
    //
    // SOUNDNESS (Phase 9 audit, analogous to A1-A4 in SV/XState): a missing or
    // invalid property is fail-loud, not silently dropped. Previously, template
    // resolution failures emitted a warning but returned an incomplete property
    // list — the property was never checked, leading to false-positive verdicts.
    let template_registry =
        super::templates::TemplateRegistry::configured(&[]).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("the template catalog: {e}"),
            location: None,
        })?;
    let mut properties: Vec<PropertySpec> = Vec::new();
    for def in &config.properties {
        if let Some(formula_str) = def.formula_str() {
            properties.push(PropertySpec {
                name: def.id.clone(),
                kind: PropertyKind::Safety,
                formula: PropertyFormula::MuCalculus(formula_str.to_string()),
                role: PropertyRole::Standalone,
                over: def.over.clone(),
                description: None,
            });
        } else if let Some(tref) = &def.template_ref {
            match template_registry.instantiate(tref) {
                Ok(inst) => {
                    properties.push(PropertySpec {
                        name: def.id.clone(),
                        kind: inst.kind,
                        formula: PropertyFormula::MuCalculus(inst.formula),
                        role: inst.role,
                        over: def.over.clone(),
                        description: None,
                    });
                }
                Err(e) => {
                    return Err(AdapterError {
                        kind: AdapterErrorKind::ParseError,
                        message: format!(
                            "property '{}' references unknown template '{}': {e}. \
                             Add the template to the registry or replace `template_ref` with a raw `formula`.",
                            def.id, tref.template
                        ),
                        location: None,
                    });
                }
            }
        } else {
            return Err(AdapterError {
                kind: AdapterErrorKind::ParseError,
                message: format!(
                    "property '{}' declares neither `formula` nor `template_ref` — \
                     cannot translate. Add one of the two fields.",
                    def.id
                ),
                location: None,
            });
        }
    }

    // Build controller(s)
    let controller = config.controllers.first().map(|def| ControllerSpec {
        name: def.name.clone(),
        source_automaton: def.source.clone(),
        formula_name: def.satisfying.clone(),
    });

    // Build description with provenance
    let description = build_description(spec, mode);

    Ok(AdapterIR {
        metadata: Metadata {
            title: effective_name,
            source_format: SourceFormat::Extraction,
            description: Some(description),
            game_semantics: None,
            known_status: None,
        },
        signals: vec![],
        automata,
        compositions,
        properties,
        controller,
    })
}

/// Build an `AutomatonSpec` from a declarative automaton definition,
/// filtering transitions by mode.
fn build_automaton(
    def: &AutomatonDef,
    mode: &str,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<AutomatonSpec, AdapterError> {
    // Build states
    let any_initial = def.states.iter().any(|s| s.is_initial());
    let states: Vec<StateSpec> = def
        .states
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // First state is initial by default if none explicitly marked
            let is_initial = if any_initial { s.is_initial() } else { i == 0 };
            StateSpec {
                name: s.name().to_string(),
                is_initial,
                valuations: None,
                three_valued: None,
            }
        })
        .collect();

    if states.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::IrConsistencyError,
            message: format!("Automaton '{}' has no states", def.id),
            location: None,
        });
    }

    // Filter transitions by mode
    let transitions: Vec<TransitionSpec> = def
        .transitions
        .iter()
        .filter(|t| t.mode == "both" || t.mode == mode)
        .map(|t| TransitionSpec {
            source: t.from.clone(),
            target: t.to.clone(),
            labels: vec![t.label.clone()],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        })
        .collect();

    if transitions.is_empty() {
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: format!(
                "Automaton '{}' has no transitions in mode '{}'",
                def.id, mode
            ),
            location: None,
        });
    }

    Ok(AutomatonSpec {
        name: def.id.clone(),
        states,
        transitions,
        controllable_labels: def.controllable_labels.clone(),
        internal_labels: vec![],
    })
}

/// Build provenance description from spec metadata.
fn build_description(spec: &ExtractionSpec, mode: &str) -> String {
    let mut parts = Vec::new();

    parts.push(format!("Translated from extraction spec (mode: {mode})"));

    if let Some(file) = &spec.source.file {
        if let Some(commit) = &spec.source.commit {
            let short = if commit.len() > 12 {
                &commit[..12]
            } else {
                commit
            };
            parts.push(format!("Source: {file} @ {short}"));
        } else {
            parts.push(format!("Source: {file}"));
        }
    }

    if let Some(cve) = &spec.source.cve {
        parts.push(format!("CVE: {cve}"));
    }

    if let Some(issue) = &spec.source.issue {
        parts.push(format!("Issue: {issue}"));
    }

    if let Some(fix_pr) = &spec.source.fix_pr {
        parts.push(format!("Fix PR: {fix_pr}"));
    }

    if let Some(fix_commit) = &spec.source.fix_commit {
        let short = if fix_commit.len() > 12 {
            &fix_commit[..12]
        } else {
            fix_commit
        };
        parts.push(format!("Fix commit: {short}"));
    }

    parts.join(" | ")
}

/// Build machine-parseable provenance header for the generated CTXDSL.
///
/// These headers enable CI to distinguish generated-from-spec files from
/// hand-written ones, and to trace CTXDSL back to specific source code.
fn build_provenance_header(spec: &ExtractionSpec, mode: &str) -> String {
    let mut lines = Vec::new();
    lines.push("// @generated-from: extraction_spec_v1".to_string());

    if let Some(file) = &spec.source.file {
        lines.push(format!("// @source-file: {file}"));
    }
    if let Some(commit) = &spec.source.commit {
        lines.push(format!("// @commit: {commit}"));
    }
    if let Some(repo) = &spec.source.repo {
        lines.push(format!("// @repo: {repo}"));
    }
    lines.push(format!("// @mode: {mode}"));

    if let Some(cve) = &spec.source.cve {
        lines.push(format!("// @cve: {cve}"));
    }
    if let Some(ghsa) = &spec.source.ghsa {
        lines.push(format!("// @ghsa: {ghsa}"));
    }
    if let Some(issue) = &spec.source.issue {
        lines.push(format!("// @issue: {issue}"));
    }
    if let Some(fix_pr) = &spec.source.fix_pr {
        lines.push(format!("// @fix-pr: {fix_pr}"));
    }
    if let Some(fix_commit) = &spec.source.fix_commit {
        lines.push(format!("// @fix-commit: {fix_commit}"));
    }

    // Attack chain summary (if bugs documented)
    for bug in &spec.bugs {
        if let Some(desc) = &bug.description {
            let short = if desc.len() > 100 {
                format!("{}...", &desc[..97])
            } else {
                desc.clone()
            };
            lines.push(format!("// @bug: {} — {}", bug.id, short));
        }
    }

    // Compositional minimization hints (L16): when multiple automata exist,
    // identify labels that are private to each automaton (candidates for hiding).
    if spec.model_config.automata.len() > 1 {
        let mut label_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for aut in &spec.model_config.automata {
            let mut aut_labels = std::collections::HashSet::new();
            for t in &aut.transitions {
                aut_labels.insert(t.label.as_str());
            }
            for l in &aut_labels {
                *label_counts.entry(l).or_insert(0) += 1;
            }
        }
        let private: Vec<&str> = label_counts
            .iter()
            .filter(|(_, count)| **count == 1)
            .map(|(label, _)| *label)
            .collect();
        if !private.is_empty() {
            let mut sorted = private;
            sorted.sort();
            lines.push(format!("// @minimize-candidates: {}", sorted.join(", ")));
        }
    }

    lines.push("//".to_string());
    lines.push(String::new());
    lines.join("\n")
}

/// Count distinct labels in the spec.
fn count_labels(spec: &ExtractionSpec) -> usize {
    let mut labels = std::collections::HashSet::new();
    for l in &spec.model_config.controllable_labels {
        labels.insert(l.as_str());
    }
    for l in &spec.model_config.uncontrollable_labels {
        labels.insert(l.as_str());
    }
    for aut in &spec.model_config.automata {
        for t in &aut.transitions {
            labels.insert(&t.label);
        }
    }
    labels.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate_spec(json: &str, mode: &str) -> AdapterOutput {
        let options = AdapterOptions {
            mode: Some(mode.to_string()),
            ..Default::default()
        };
        ExtractionAdapter::translate(json, &options).expect("translation should succeed")
    }

    #[test]
    fn detect_extraction_spec() {
        assert!(ExtractionAdapter::detect(
            r#"{"$schema": "extraction_spec_v1", "source": {}, "model_config": {"context_name": "t"}}"#
        ));
        assert!(!ExtractionAdapter::detect(
            r#"{"id": "test", "initial": "s0", "states": {}}"#
        ));
        assert!(!ExtractionAdapter::detect("INFO { TITLE: \"test\" }"));
    }

    #[test]
    fn translate_simple_spec() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {"repo": "test/repo", "commit": "abc123def456"},
            "model_config": {
                "context_name": "simple_test",
                "automata": [
                    {
                        "id": "Main",
                        "states": [
                            {"name": "Idle", "initial": true},
                            {"name": "Active"}
                        ],
                        "controllable_labels": ["ev_go"],
                        "transitions": [
                            {"from": "Idle", "to": "Active", "label": "ev_go"},
                            {"from": "Active", "to": "Active", "label": "ev_go"},
                            {"from": "Idle", "to": "Idle", "label": "noop"},
                            {"from": "Active", "to": "Active", "label": "noop"}
                        ]
                    }
                ],
                "properties": [
                    {
                        "id": "safety",
                        "formula": "nu X. ([] X)",
                        "over": "Main"
                    }
                ]
            }
        }"#;

        let output = translate_spec(json, "vulnerable");
        assert_eq!(output.source_info.format, SourceFormat::Extraction);
        assert!(!output.ctxdsl.is_empty());
        assert!(output.ctxdsl.contains("context simple_test"));
        assert!(output.ctxdsl.contains("automaton Main"));
        assert!(
            output
                .ctxdsl
                .contains("transition Idle -> Active on label ev_go")
        );

        // Parse and realize to validate
        let doc = crate::context_dsl::parse(&output.ctxdsl).unwrap();
        let realized = crate::context_dsl::realize_context(&doc, &[]).unwrap();
        let clts = realized.context.clts("Main").expect("Main automaton");
        assert_eq!(clts.state_count(), 2);
    }

    #[test]
    fn mode_filters_transitions() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "mode_test",
                "automata": [{
                    "id": "A",
                    "states": [
                        {"name": "Open", "initial": true},
                        {"name": "Closed"}
                    ],
                    "transitions": [
                        {"from": "Open", "to": "Closed", "label": "ev_close"},
                        {"from": "Closed", "to": "Closed", "label": "ev_request", "mode": "vulnerable"},
                        {"from": "Open", "to": "Open", "label": "noop"},
                        {"from": "Closed", "to": "Closed", "label": "noop"}
                    ]
                }]
            }
        }"#;

        // Vulnerable mode: ev_request transition exists in Closed
        let vuln = translate_spec(json, "vulnerable");
        assert!(vuln.ctxdsl.contains("ev_request"));

        // Fixed mode: ev_request transition filtered out
        let fixed = translate_spec(json, "fixed");
        assert!(!fixed.ctxdsl.contains("ev_request"));
    }

    /// GAP-002: arbitrary mode strings (e.g. investigation framings other than
    /// the bug/fix dichotomy) are accepted iff at least one transition in the
    /// loaded spec is tagged with that mode. The per-transition filter then
    /// keeps transitions whose `mode` is `"both"` or matches the requested mode.
    #[test]
    fn mode_accepts_custom_string() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "custom_mode_test",
                "automata": [{
                    "id": "A",
                    "states": [
                        {"name": "S0", "initial": true},
                        {"name": "S1"}
                    ],
                    "transitions": [
                        {"from": "S0", "to": "S1", "label": "ev_audited", "mode": "as_audited"},
                        {"from": "S0", "to": "S1", "label": "ev_cached", "mode": "with_provider_cache"},
                        {"from": "S0", "to": "S0", "label": "ev_shared", "mode": "both"},
                        {"from": "S1", "to": "S1", "label": "noop"},
                        {"from": "S0", "to": "S0", "label": "noop"}
                    ]
                }]
            }
        }"#;

        let output = translate_spec(json, "as_audited");
        assert!(
            output.ctxdsl.contains("ev_audited"),
            "as_audited transition must be kept under --mode as_audited"
        );
        assert!(
            output.ctxdsl.contains("ev_shared"),
            "`mode: \"both\"` transition must always be kept"
        );
        assert!(
            !output.ctxdsl.contains("ev_cached"),
            "with_provider_cache transition must be filtered out under --mode as_audited"
        );
    }

    /// GAP-002: an unknown mode value produces an error message that lists the
    /// valid modes for the loaded spec (sorted), so authors can self-correct.
    #[test]
    fn mode_rejects_unknown_mode() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "unknown_mode_test",
                "automata": [{
                    "id": "A",
                    "states": [
                        {"name": "S0", "initial": true},
                        {"name": "S1"}
                    ],
                    "transitions": [
                        {"from": "S0", "to": "S1", "label": "ev_audited", "mode": "as_audited"},
                        {"from": "S0", "to": "S1", "label": "ev_cached", "mode": "with_provider_cache"},
                        {"from": "S0", "to": "S0", "label": "ev_shared", "mode": "both"}
                    ]
                }]
            }
        }"#;

        let options = AdapterOptions {
            mode: Some("unknown_xyz".to_string()),
            ..Default::default()
        };
        let result = ExtractionAdapter::translate(json, &options);
        assert!(
            result.is_err(),
            "unknown mode should produce an AdapterError"
        );
        let err = result.err().unwrap();
        assert!(
            err.message.contains("unknown_xyz"),
            "error must reference the bad mode: {}",
            err.message
        );
        // The valid-modes list is the union of universal defaults
        // (fixed, vulnerable, both) and the spec's per-transition tags
        // (as_audited, with_provider_cache, both). Sorted: as_audited, both,
        // fixed, vulnerable, with_provider_cache.
        for expected in [
            "as_audited",
            "both",
            "fixed",
            "vulnerable",
            "with_provider_cache",
        ] {
            assert!(
                err.message.contains(expected),
                "error message should list valid mode '{expected}'. Got: {}",
                err.message
            );
        }
    }

    /// GAP-002: with no `--mode` flag, the default remains `"vulnerable"` so
    /// existing CLI invocations and specs that rely on the default keep working.
    #[test]
    fn mode_default_still_vulnerable() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "default_mode_test",
                "automata": [{
                    "id": "A",
                    "states": [
                        {"name": "Open", "initial": true},
                        {"name": "Closed"}
                    ],
                    "transitions": [
                        {"from": "Open", "to": "Closed", "label": "ev_close"},
                        {"from": "Closed", "to": "Closed", "label": "ev_vuln_only", "mode": "vulnerable"},
                        {"from": "Closed", "to": "Closed", "label": "ev_fix_only", "mode": "fixed"},
                        {"from": "Open", "to": "Open", "label": "noop"},
                        {"from": "Closed", "to": "Closed", "label": "noop"}
                    ]
                }]
            }
        }"#;

        // No `mode` field set on AdapterOptions → default applies.
        let options = AdapterOptions::default();
        let output = ExtractionAdapter::translate(json, &options)
            .expect("default mode (vulnerable) should be accepted");
        assert!(
            output.ctxdsl.contains("@mode: vulnerable"),
            "default mode must be 'vulnerable'"
        );
        assert!(
            output.ctxdsl.contains("ev_vuln_only"),
            "vulnerable transition must be kept under default mode"
        );
        assert!(
            !output.ctxdsl.contains("ev_fix_only"),
            "fixed-only transition must be filtered out under default (vulnerable) mode"
        );
    }

    /// GAP-002: the universal defaults `fixed`, `vulnerable`, `both` are always
    /// accepted even if the spec only uses custom tags. They simply select the
    /// implicit-`"both"` transitions in that case.
    #[test]
    fn mode_universal_defaults() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "universal_defaults_test",
                "automata": [{
                    "id": "A",
                    "states": [
                        {"name": "S0", "initial": true},
                        {"name": "S1"}
                    ],
                    "transitions": [
                        {"from": "S0", "to": "S1", "label": "ev_audited", "mode": "as_audited"},
                        {"from": "S0", "to": "S1", "label": "ev_cached", "mode": "with_provider_cache"},
                        {"from": "S0", "to": "S0", "label": "ev_always"},
                        {"from": "S1", "to": "S1", "label": "noop"},
                        {"from": "S0", "to": "S0", "label": "noop"}
                    ]
                }]
            }
        }"#;

        // `--mode fixed` is a universal default — the spec uses no `fixed`
        // tags, so only the implicit-`both` transitions remain.
        let output = translate_spec(json, "fixed");
        assert!(
            output.ctxdsl.contains("ev_always"),
            "implicit-`both` transition must be kept under universal default --mode fixed"
        );
        assert!(
            !output.ctxdsl.contains("ev_audited"),
            "custom-tagged transition must be filtered out under --mode fixed"
        );
        assert!(
            !output.ctxdsl.contains("ev_cached"),
            "custom-tagged transition must be filtered out under --mode fixed"
        );
    }

    #[test]
    fn composition_emitted() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "comp_test",
                "automata": [
                    {
                        "id": "A",
                        "states": [{"name": "S0", "initial": true}],
                        "transitions": [{"from": "S0", "to": "S0", "label": "ev_x"}]
                    },
                    {
                        "id": "B",
                        "states": [{"name": "T0", "initial": true}],
                        "transitions": [{"from": "T0", "to": "T0", "label": "ev_x"}]
                    }
                ],
                "composition": {
                    "type": "asynchronous",
                    "name": "system",
                    "members": ["A", "B"]
                }
            }
        }"#;

        let output = translate_spec(json, "vulnerable");
        assert!(output.ctxdsl.contains("asynchronous system"));
        assert!(output.ctxdsl.contains("members [A, B]"));
    }

    #[test]
    fn properties_with_over() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "prop_test",
                "automata": [
                    {
                        "id": "Main",
                        "states": [{"name": "S0", "initial": true}, {"name": "Bad"}],
                        "transitions": [
                            {"from": "S0", "to": "Bad", "label": "ev_fail", "mode": "vulnerable"},
                            {"from": "S0", "to": "S0", "label": "noop"},
                            {"from": "Bad", "to": "Bad", "label": "noop"}
                        ]
                    }
                ],
                "properties": [
                    {
                        "id": "no_bad",
                        "formula": "nu X. ((!Bad) && ([] X))",
                        "over": "Main",
                        "holds_in_fixed": true,
                        "holds_in_vulnerable": false
                    }
                ]
            }
        }"#;

        let output = translate_spec(json, "vulnerable");
        assert!(output.ctxdsl.contains("formula no_bad"));
        assert!(output.ctxdsl.contains("over Main"));
        assert!(output.ctxdsl.contains("nu X. ((!Bad) && ([] X))"));
    }

    #[test]
    fn controller_emitted() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "ctrl_test",
                "automata": [{
                    "id": "System",
                    "states": [{"name": "S0", "initial": true}],
                    "controllable_labels": ["ev_act"],
                    "transitions": [{"from": "S0", "to": "S0", "label": "ev_act"}]
                }],
                "properties": [{"id": "safe", "formula": "nu X. ([] X)", "over": "System"}],
                "controllers": [{"name": "enforcer", "source": "System", "satisfying": "safe"}]
            }
        }"#;

        let output = translate_spec(json, "vulnerable");
        assert!(output.ctxdsl.contains("controller enforcer"));
        assert!(output.ctxdsl.contains("source System"));
        assert!(output.ctxdsl.contains("satisfying safe"));
    }

    #[test]
    fn provenance_headers_emitted() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {
                "repo": "owner/repo",
                "commit": "abc123def456789",
                "file": "src/server.ts",
                "cve": "CVE-2026-99999",
                "issue": "Transport reuse bug"
            },
            "bugs": [{"id": "bug1", "description": "Missing guard on close"}],
            "model_config": {
                "context_name": "prov_test",
                "automata": [{
                    "id": "A",
                    "states": [{"name": "S0", "initial": true}],
                    "transitions": [{"from": "S0", "to": "S0", "label": "noop"}]
                }]
            }
        }"#;

        let output = translate_spec(json, "vulnerable");
        assert!(
            output
                .ctxdsl
                .contains("// @generated-from: extraction_spec_v1")
        );
        assert!(output.ctxdsl.contains("// @source-file: src/server.ts"));
        assert!(output.ctxdsl.contains("// @commit: abc123def456789"));
        assert!(output.ctxdsl.contains("// @repo: owner/repo"));
        assert!(output.ctxdsl.contains("// @mode: vulnerable"));
        assert!(output.ctxdsl.contains("// @cve: CVE-2026-99999"));
        assert!(output.ctxdsl.contains("// @bug: bug1"));
    }

    #[test]
    fn template_ref_resolves_to_formula() {
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "template_test",
                "automata": [{
                    "id": "FSM",
                    "states": [
                        {"name": "Idle", "initial": true},
                        {"name": "Active"},
                        {"name": "Dead"}
                    ],
                    "transitions": [
                        {"from": "Idle", "to": "Active", "label": "ev_go"},
                        {"from": "Active", "to": "Dead", "label": "ev_die"},
                        {"from": "Idle", "to": "Idle", "label": "noop"},
                        {"from": "Active", "to": "Active", "label": "noop"}
                    ]
                }],
                "properties": [
                    {
                        "id": "deadlock_free",
                        "template_ref": {"template": "no_deadlock"},
                        "over": "FSM"
                    },
                    {
                        "id": "can_reach_dead",
                        "template_ref": {"template": "reachable", "args": {"TARGET": "Dead"}},
                        "over": "FSM"
                    },
                    {
                        "id": "raw_formula_wins",
                        "formula": "nu X. ([] X)",
                        "template_ref": {"template": "reachable", "args": {"TARGET": "Idle"}},
                        "over": "FSM"
                    }
                ]
            }
        }"#;

        let output = translate_spec(json, "vulnerable");

        // Template "no_deadlock" should produce the formula
        assert!(output.ctxdsl.contains("formula deadlock_free"));
        assert!(output.ctxdsl.contains("<> true"));

        // Template "reachable" with TARGET=Dead should produce the formula
        assert!(output.ctxdsl.contains("formula can_reach_dead"));
        assert!(output.ctxdsl.contains("Dead"));

        // Raw formula should take precedence over template_ref
        assert!(output.ctxdsl.contains("formula raw_formula_wins"));
        assert!(output.ctxdsl.contains("nu X. ([] X)"));

        // Verify the generated CTXDSL parses and realizes successfully
        let doc = crate::context_dsl::parse(&output.ctxdsl).unwrap();
        let realized = crate::context_dsl::realize_context(&doc, &[]).unwrap();
        assert!(
            realized.context.clts("FSM").is_some(),
            "FSM automaton should exist"
        );
        assert!(
            realized.formulas.contains_key("deadlock_free"),
            "deadlock_free formula should exist"
        );
        assert!(
            realized.formulas.contains_key("can_reach_dead"),
            "can_reach_dead formula should exist"
        );
    }

    #[test]
    fn template_ref_with_unknown_template_is_error() {
        // Phase 9 (Soundness audit): unknown templates were previously silently
        // dropped, leading to false-positive verdicts where the property was
        // never checked. The fix is fail-loud: an `AdapterError::ParseError`.
        let json = r#"{
            "$schema": "extraction_spec_v1",
            "source": {},
            "model_config": {
                "context_name": "warn_test",
                "automata": [{
                    "id": "A",
                    "states": [{"name": "S0", "initial": true}],
                    "transitions": [{"from": "S0", "to": "S0", "label": "noop"}]
                }],
                "properties": [
                    {
                        "id": "bad_template",
                        "template_ref": {"template": "nonexistent_template"}
                    }
                ]
            }
        }"#;

        let options = AdapterOptions {
            mode: Some("vulnerable".to_string()),
            ..Default::default()
        };
        let result = ExtractionAdapter::translate(json, &options);
        assert!(
            result.is_err(),
            "Unknown template_ref must produce an AdapterError, not silent drop. \
             Got: {:?}",
            result.map(|o| o.warnings.len())
        );
        if let Err(e) = result {
            assert!(
                e.message.contains("template") || e.message.contains("nonexistent_template"),
                "Error message should reference the missing template. Got: {}",
                e.message
            );
        }
    }
}
