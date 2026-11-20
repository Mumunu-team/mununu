//! AST-based extraction — configuration, domain profiles, state space derivation,
//! and (behind `ast-extract` feature) tree-sitter parsing and extraction.
//!
//! Shared types (config, domain, call_summary, state_space) are always available.
//! Tree-sitter dependent code (parser, extractor, extract_from_source) requires
//! the `ast-extract` feature flag.

pub mod call_summary;
pub mod config;
pub mod domain;
pub mod state_space;

#[cfg(feature = "ast-extract")]
#[allow(clippy::collapsible_if)]
pub mod concurrency_detect;
#[cfg(feature = "ast-extract")]
#[allow(clippy::collapsible_if)]
pub mod extractor;
#[cfg(feature = "ast-extract")]
pub mod parser;

/// Extract a model from source code using the AST-based extraction pipeline.
///
/// This is the main entry point for in-process extraction. It:
/// 1. Parses the extraction config (.extract.json)
/// 2. Parses the source file via tree-sitter
/// 3. Extracts fields, methods, guards, effects per target
/// 4. Derives automata via state space enumeration
/// 5. Returns a complete ExtractionSpec (.espec.json)
///
/// Requires the `ast-extract` feature flag.
#[cfg(feature = "ast-extract")]
pub fn extract_from_source(
    config_json: &str,
    source_code: &str,
    language: &str,
) -> Result<super::ast::ExtractionSpec, String> {
    let config: config::ExtractionConfig = serde_json::from_str(config_json)
        .map_err(|e| format!("Failed to parse extraction config: {e}"))?;

    let lang = language
        .parse::<String>()
        .ok()
        .and_then(|s| parser::SourceLanguage::from_name(&s))
        .or_else(|| {
            config
                .language
                .as_deref()
                .and_then(parser::SourceLanguage::from_name)
        })
        .ok_or_else(|| {
            format!(
                "Unknown language: {language} (expected one of: {})",
                parser::SourceLanguage::NAMES.join(", ")
            )
        })?;

    let parsed = parser::parse_source(source_code, lang)?;

    let profile = config.domain.as_deref().and_then(domain::get_profile);

    let mut all_automata = Vec::new();
    let mut all_warnings = Vec::new();

    // Compositional path (Phase A): when `composition.instances` is
    // non-empty, produce one automaton per instance, looking up the
    // matching target by `instance.of == target.class`. Apply per-instance
    // label rewriting with `composition.shared` as the synchronization set.
    // Legacy path (instances empty): one automaton per target, no rewriting.
    let instance_driven = config
        .composition
        .as_ref()
        .map(|c| !c.instances.is_empty())
        .unwrap_or(false);

    if instance_driven {
        let comp = config.composition.as_ref().unwrap();
        let shared: std::collections::HashSet<&str> =
            comp.shared.iter().map(String::as_str).collect();
        for instance in &comp.instances {
            let target = config
                .targets
                .iter()
                .find(|t| t.class == instance.of)
                .ok_or_else(|| {
                    format!(
                        "composition.instances declares an instance of class '{}' \
                         but no matching target with that class was found",
                        instance.of
                    )
                })?;
            let (mut automaton_def, target_warnings) =
                build_automaton_def(&parsed, target, profile, Some(&instance.as_))?;
            rewrite_labels_for_instance(&mut automaton_def, &instance.as_, &shared);
            all_automata.push(automaton_def);
            all_warnings.extend(target_warnings);
        }
        // GAP-008: emit hand-modeled shared resources as additional
        // automata. They sit alongside the per-instance automata in the
        // composition's `members` list. Resource labels are NOT
        // per-instance-prefixed — they're authored verbatim, so a label
        // matching one declared in `composition.shared` (or one on an
        // instance automaton) synchronizes via alphabet intersection.
        for resource in &comp.resources {
            let resource_def = build_resource_automaton_def(resource)?;
            all_automata.push(resource_def);
        }
    } else {
        for target in &config.targets {
            let (automaton_def, target_warnings) =
                build_automaton_def(&parsed, target, profile, None)?;
            all_automata.push(automaton_def);
            all_warnings.extend(target_warnings);
        }
    }

    // Surface accumulated warnings to stderr. Mirrors the existing
    // `eprintln!` pattern used by the state-space size guards in
    // `state_space::derive_automaton`.
    for warning in &all_warnings {
        eprintln!("{warning}");
    }
    if !all_warnings.is_empty() {
        eprintln!("Extracted with {} warnings.", all_warnings.len());
    }

    let context_name = config.context_name.clone().unwrap_or_else(|| {
        config
            .targets
            .first()
            .map(|t| t.class.to_lowercase())
            .unwrap_or_else(|| "extracted".to_string())
    });

    let composition = config
        .composition
        .as_ref()
        .map(|c| super::ast::CompositionDef {
            type_: c.type_.clone(),
            name: c.name.clone(),
            members: all_automata.iter().map(|a| a.id.clone()).collect(),
        });

    let properties: Vec<super::ast::PropertyDef> = config
        .properties
        .iter()
        .map(|p| super::ast::PropertyDef {
            id: p.id.clone(),
            description: p.description.clone(),
            formula: Some(p.formula.clone()),
            formula_template: None,
            template_ref: None,
            over: p.over.clone(),
            holds_in_fixed: None,
            holds_in_vulnerable: None,
        })
        .collect();

    Ok(super::ast::ExtractionSpec {
        schema: Some("extraction_spec_v1".to_string()),
        source: super::ast::SourceRef {
            repo: config.source.repo.clone(),
            commit: config.source.commit.clone(),
            file: Some(config.source.file.clone()),
            class: config.targets.first().map(|t| t.class.clone()),
            cve: None,
            ghsa: None,
            issue: None,
            fix_pr: None,
            fix_commit: None,
        },
        state_fields: vec![],
        methods: vec![],
        bugs: vec![],
        model_config: super::ast::ModelConfig {
            context_name,
            controllable_labels: all_automata
                .iter()
                .flat_map(|a| a.controllable_labels.clone())
                .collect(),
            uncontrollable_labels: vec![],
            automata: all_automata,
            composition,
            properties,
            controllers: vec![],
        },
    })
}

/// Build an `AutomatonDef` for one target. Pulled out of the main loop in
/// `extract_from_source` so the legacy "one automaton per target" path and
/// the new compositional "one automaton per instance" path share the same
/// extract → derive → assemble pipeline.
///
/// `automaton_id_override` is `Some(instance_name)` for compositional
/// extraction (the instance's `as` value becomes the automaton id), `None`
/// for the legacy path (the target's `automaton_id` or `class` is used).
#[cfg(feature = "ast-extract")]
fn build_automaton_def(
    parsed: &parser::ParsedSource,
    target: &config::TargetConfig,
    profile: Option<&domain::DomainProfile>,
    automaton_id_override: Option<&str>,
) -> Result<(super::ast::AutomatonDef, Vec<String>), String> {
    let extracted = extractor::extract_target(parsed, target, profile)?;
    let label_prefix = profile.map(|p| p.label_naming.prefix).unwrap_or("ev_");
    let add_noop = profile.map(|p| p.add_noop_self_loops).unwrap_or(true);

    let id = automaton_id_override.unwrap_or(&extracted.automaton_id);
    let derived = state_space::derive_automaton(
        id,
        &extracted.fields,
        &extracted.methods,
        &target.state_names,
        label_prefix,
        add_noop,
    );

    let automaton_def = super::ast::AutomatonDef {
        id: derived.name.clone(),
        states: derived
            .states
            .iter()
            .map(|s| {
                super::ast::StateDef::Structured(super::ast::StateDefStructured {
                    name: s.name.clone(),
                    initial: s.is_initial,
                })
            })
            .collect(),
        controllable_labels: derived.controllable_labels.clone(),
        transitions: derived
            .transitions
            .iter()
            .map(|t| super::ast::TransitionDef {
                from: t.from.clone(),
                to: t.to.clone(),
                label: t.label.clone(),
                mode: "both".to_string(),
                derived_from: None,
                comment: None,
            })
            .collect(),
        fields: vec![],
        note: None,
        role: None,
    };

    let mut warnings = extracted.warnings;
    warnings.extend(derived.warnings);
    Ok((automaton_def, warnings))
}

/// GAP-008: build an `AutomatonDef` from a hand-modeled `ResourceDecl`
/// (no source scanning). Resources are authored declaratively in the
/// extract config; this function converts the declarative form into the
/// existing `AutomatonDef` shape used by the composition emit pipeline.
///
/// Validation: the `initial` state must be present in `states`, and every
/// transition's `from` / `to` must reference declared states. A bad spec
/// fails extraction with a clear actionable error rather than silently
/// producing a malformed automaton.
#[cfg(feature = "ast-extract")]
fn build_resource_automaton_def(
    resource: &config::ResourceDecl,
) -> Result<super::ast::AutomatonDef, String> {
    let states_set: std::collections::HashSet<&str> =
        resource.states.iter().map(String::as_str).collect();
    if !states_set.contains(resource.initial.as_str()) {
        return Err(format!(
            "composition.resources['{}'].initial = '{}' is not in `states` (declared: {})",
            resource.name,
            resource.initial,
            resource.states.join(", ")
        ));
    }
    for (i, t) in resource.transitions.iter().enumerate() {
        if !states_set.contains(t.from.as_str()) {
            return Err(format!(
                "composition.resources['{}'].transitions[{}].from = '{}' \
                 is not in `states` (declared: {})",
                resource.name,
                i,
                t.from,
                resource.states.join(", ")
            ));
        }
        if !states_set.contains(t.to.as_str()) {
            return Err(format!(
                "composition.resources['{}'].transitions[{}].to = '{}' \
                 is not in `states` (declared: {})",
                resource.name,
                i,
                t.to,
                resource.states.join(", ")
            ));
        }
    }

    let states = resource
        .states
        .iter()
        .map(|name| {
            super::ast::StateDef::Structured(super::ast::StateDefStructured {
                name: name.clone(),
                initial: name == &resource.initial,
            })
        })
        .collect();

    let transitions = resource
        .transitions
        .iter()
        .map(|t| super::ast::TransitionDef {
            from: t.from.clone(),
            to: t.to.clone(),
            label: t.label.clone(),
            mode: "both".to_string(),
            derived_from: None,
            comment: None,
        })
        .collect();

    Ok(super::ast::AutomatonDef {
        id: resource.name.clone(),
        states,
        controllable_labels: resource.controllable_labels.clone(),
        transitions,
        fields: vec![],
        note: Some(format!(
            "Hand-modeled resource declared via composition.resources[\"{}\"]",
            resource.name
        )),
        role: None,
    })
}

/// Rewrite labels on an automaton for compositional extraction. Every label
/// `L` not in the `shared` set becomes `<instance_name>__<L>`. Labels in
/// `shared` are kept verbatim, becoming the synchronization points across
/// instances (the existing composition engine's alphabet-intersection logic
/// then forces them to fire together).
///
/// Pure function operating on a single automaton's transitions and
/// controllable_labels. The `noop` label is a special case used by the
/// state-space engine to keep states reachable; it is always per-instance-
/// prefixed (never shared) so noops don't accidentally synchronize across
/// instances, which would prevent independent progress.
#[cfg(feature = "ast-extract")]
fn rewrite_labels_for_instance(
    automaton: &mut super::ast::AutomatonDef,
    instance_name: &str,
    shared: &std::collections::HashSet<&str>,
) {
    fn rewrite(label: &str, instance: &str, shared: &std::collections::HashSet<&str>) -> String {
        if label == "noop" || !shared.contains(label) {
            format!("{instance}__{label}")
        } else {
            label.to_string()
        }
    }

    for transition in &mut automaton.transitions {
        transition.label = rewrite(&transition.label, instance_name, shared);
    }
    for label in &mut automaton.controllable_labels {
        *label = rewrite(label, instance_name, shared);
    }
}

#[cfg(feature = "ast-extract")]
#[cfg(test)]
mod compositional_tests {
    use super::*;

    fn sample_automaton(id: &str) -> super::super::ast::AutomatonDef {
        super::super::ast::AutomatonDef {
            id: id.to_string(),
            states: vec![],
            controllable_labels: vec!["save".to_string(), "internal_op".to_string()],
            transitions: vec![
                super::super::ast::TransitionDef {
                    from: "s0".to_string(),
                    to: "s1".to_string(),
                    label: "save".to_string(),
                    mode: "both".to_string(),
                    derived_from: None,
                    comment: None,
                },
                super::super::ast::TransitionDef {
                    from: "s1".to_string(),
                    to: "s1".to_string(),
                    label: "internal_op".to_string(),
                    mode: "both".to_string(),
                    derived_from: None,
                    comment: None,
                },
                super::super::ast::TransitionDef {
                    from: "s0".to_string(),
                    to: "s0".to_string(),
                    label: "noop".to_string(),
                    mode: "both".to_string(),
                    derived_from: None,
                    comment: None,
                },
            ],
            fields: vec![],
            note: None,
            role: None,
        }
    }

    #[test]
    fn compose_two_instances_no_shared() {
        // shared: empty — every label gets per-instance prefix, including
        // labels with the same name across instances. Result: completely
        // independent (no synchronization).
        let mut a = sample_automaton("worker_a");
        let mut b = sample_automaton("worker_b");
        let shared: std::collections::HashSet<&str> = std::collections::HashSet::new();
        rewrite_labels_for_instance(&mut a, "worker_a", &shared);
        rewrite_labels_for_instance(&mut b, "worker_b", &shared);

        let a_labels: Vec<&String> = a.transitions.iter().map(|t| &t.label).collect();
        let b_labels: Vec<&String> = b.transitions.iter().map(|t| &t.label).collect();

        assert!(a_labels.contains(&&"worker_a__save".to_string()));
        assert!(a_labels.contains(&&"worker_a__internal_op".to_string()));
        assert!(a_labels.contains(&&"worker_a__noop".to_string()));
        assert!(b_labels.contains(&&"worker_b__save".to_string()));
        // No label is shared between the two instances.
        for la in &a_labels {
            assert!(
                !b_labels.contains(la),
                "label {la} should not appear in worker_b after rewriting"
            );
        }
        // Controllable labels are also rewritten.
        assert_eq!(
            a.controllable_labels,
            vec![
                "worker_a__save".to_string(),
                "worker_a__internal_op".to_string()
            ]
        );
    }

    #[test]
    fn compose_two_instances_with_shared() {
        // shared: ["save"] — both instances keep `save` verbatim, so they
        // synchronize on it; `internal_op` gets per-instance prefix.
        let mut a = sample_automaton("worker_a");
        let mut b = sample_automaton("worker_b");
        let shared: std::collections::HashSet<&str> = ["save"].iter().copied().collect();
        rewrite_labels_for_instance(&mut a, "worker_a", &shared);
        rewrite_labels_for_instance(&mut b, "worker_b", &shared);

        let a_labels: Vec<&String> = a.transitions.iter().map(|t| &t.label).collect();
        let b_labels: Vec<&String> = b.transitions.iter().map(|t| &t.label).collect();

        // `save` is shared verbatim.
        assert!(a_labels.contains(&&"save".to_string()));
        assert!(b_labels.contains(&&"save".to_string()));
        // `internal_op` is per-instance.
        assert!(a_labels.contains(&&"worker_a__internal_op".to_string()));
        assert!(b_labels.contains(&&"worker_b__internal_op".to_string()));
        assert!(!a_labels.contains(&&"worker_b__internal_op".to_string()));
        // `noop` is never shared, even if accidentally listed.
        assert!(a_labels.contains(&&"worker_a__noop".to_string()));
        assert!(b_labels.contains(&&"worker_b__noop".to_string()));
        // Controllable labels: shared ones verbatim, others prefixed.
        assert!(a.controllable_labels.contains(&"save".to_string()));
        assert!(
            a.controllable_labels
                .contains(&"worker_a__internal_op".to_string())
        );
    }

    #[test]
    fn compose_label_rewrite_function_unit() {
        // Edge cases: empty automaton; all-shared; noop-shared (must still
        // be prefixed); duplicate labels; empty instance name (still works
        // mechanically — the prefix becomes `__label`, not crashable).
        let mut empty = super::super::ast::AutomatonDef {
            id: "x".to_string(),
            states: vec![],
            controllable_labels: vec![],
            transitions: vec![],
            fields: vec![],
            note: None,
            role: None,
        };
        let shared: std::collections::HashSet<&str> = std::collections::HashSet::new();
        rewrite_labels_for_instance(&mut empty, "x", &shared);
        assert!(empty.transitions.is_empty());

        // All-shared: nothing gets prefixed (except noop).
        let mut a = sample_automaton("a");
        let all_shared: std::collections::HashSet<&str> =
            ["save", "internal_op"].iter().copied().collect();
        rewrite_labels_for_instance(&mut a, "a", &all_shared);
        let labels: Vec<&String> = a.transitions.iter().map(|t| &t.label).collect();
        assert!(labels.contains(&&"save".to_string()));
        assert!(labels.contains(&&"internal_op".to_string()));
        // noop is always per-instance regardless of `shared`.
        assert!(labels.contains(&&"a__noop".to_string()));

        // noop in shared: still per-instance (the rewrite explicitly
        // excludes it from sharing to preserve independent progress).
        let mut b = sample_automaton("b");
        let noop_shared: std::collections::HashSet<&str> = ["noop"].iter().copied().collect();
        rewrite_labels_for_instance(&mut b, "b", &noop_shared);
        let b_labels: Vec<&String> = b.transitions.iter().map(|t| &t.label).collect();
        assert!(b_labels.contains(&&"b__noop".to_string()));
        assert!(!b_labels.contains(&&"noop".to_string()));
    }
}
