//! End-to-end test for compositional extraction.
//!
//! Mirrors the MCP-005 mcp-server-memory file-race pattern: two worker
//! instances of the same class plus a shared resource, with the workers'
//! `save` operation as the synchronization point. Asserts the produced
//! espec has the expected per-instance label rewriting and composition
//! shape — the same structural properties the hand-modeled
//! `.claude/reviews/prospector/staging/MCP-005/file_race.espec.json`
//! baseline carries.

#![cfg(feature = "ast-extract")]

use mununu_core::adapter::extraction::ast_extract::extract_from_source;

const MINIMAL_MCP005_SOURCE: &str = r#"
class KnowledgeGraphManager {
    private state: boolean = false;
    public load(): void {
        if (this.state) {
            return;
        }
        this.state = true;
    }
    public save(): void {
        if (!this.state) {
            return;
        }
        this.state = false;
    }
}
"#;

/// Compositional extraction with two instances of the same class and one
/// shared label produces:
///   - exactly 2 automata, ids `worker_a` and `worker_b`
///   - per-instance label rewriting (each automaton's transitions carry
///     the `<instance>__` prefix on non-shared labels)
///   - the shared label kept verbatim in BOTH instances' transitions
///   - a composition declaration whose members match the instance names
#[test]
fn compositional_extract_two_instances_one_shared() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load", "save"] }
            }
        ],
        "composition": {
            "type": "asynchronous",
            "name": "race",
            "instances": [
                { "of": "KnowledgeGraphManager", "as": "worker_a" },
                { "of": "KnowledgeGraphManager", "as": "worker_b" }
            ],
            "shared": ["ev_save"]
        }
    }"#;

    let spec = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript")
        .expect("extraction should succeed");

    // Two automata, in declared instance order.
    assert_eq!(
        spec.model_config.automata.len(),
        2,
        "expected 2 automata (one per instance), got {}",
        spec.model_config.automata.len()
    );
    let ids: Vec<&str> = spec
        .model_config
        .automata
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(ids, vec!["worker_a", "worker_b"]);

    // Per-instance label rewriting:
    //   non-shared labels (`ev_load`) get the `<instance>__` prefix
    //   shared labels (`ev_save`) are kept verbatim in BOTH instances
    let worker_a = &spec.model_config.automata[0];
    let worker_b = &spec.model_config.automata[1];

    let a_labels: Vec<&str> = worker_a
        .transitions
        .iter()
        .map(|t| t.label.as_str())
        .collect();
    let b_labels: Vec<&str> = worker_b
        .transitions
        .iter()
        .map(|t| t.label.as_str())
        .collect();

    // Per-instance: worker_a has `worker_a__ev_load`, NOT `worker_b__ev_load`.
    assert!(
        a_labels.contains(&"worker_a__ev_load"),
        "worker_a should carry per-instance load label, got labels: {:?}",
        a_labels
    );
    assert!(
        !a_labels.contains(&"worker_b__ev_load"),
        "worker_a should NOT carry worker_b's labels"
    );
    assert!(
        b_labels.contains(&"worker_b__ev_load"),
        "worker_b should carry per-instance load label, got labels: {:?}",
        b_labels
    );

    // Shared: ev_save is verbatim in BOTH instances (the alphabet
    // intersection that the existing composition engine synchronizes on).
    assert!(
        a_labels.contains(&"ev_save"),
        "worker_a should carry the shared `ev_save` label verbatim, got: {:?}",
        a_labels
    );
    assert!(
        b_labels.contains(&"ev_save"),
        "worker_b should carry the shared `ev_save` label verbatim, got: {:?}",
        b_labels
    );

    // The composition declaration mirrors the instance names.
    let comp = spec
        .model_config
        .composition
        .as_ref()
        .expect("composition should be present");
    assert_eq!(comp.type_, "asynchronous");
    assert_eq!(comp.name, "race");
    assert_eq!(comp.members, vec!["worker_a", "worker_b"]);
}

/// Regression guard: legacy extract config (no `composition.instances`)
/// produces the same output it did before the schema extension —
/// one automaton per target with the target's class as the automaton id.
#[test]
fn legacy_single_target_extraction_unchanged() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load", "save"] }
            }
        ]
    }"#;

    let spec = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript")
        .expect("legacy extraction should succeed");

    assert_eq!(spec.model_config.automata.len(), 1);
    let aut = &spec.model_config.automata[0];
    assert_eq!(aut.id, "KnowledgeGraphManager");
    // Labels carry the global ev_ prefix but NO per-instance prefix.
    let labels: Vec<&str> = aut.transitions.iter().map(|t| t.label.as_str()).collect();
    assert!(labels.contains(&"ev_load"));
    assert!(labels.contains(&"ev_save"));
    for label in &labels {
        assert!(
            !label.contains("__"),
            "legacy extraction must not apply per-instance prefix, got `{label}`"
        );
    }
}

/// Compositional extraction with no `shared` (default empty): every label
/// is per-instance-prefixed, including labels with the same name across
/// instances. Result: zero alphabet intersection, full async.
#[test]
fn compositional_extract_no_shared_full_async() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load", "save"] }
            }
        ],
        "composition": {
            "type": "asynchronous",
            "name": "independent_workers",
            "instances": [
                { "of": "KnowledgeGraphManager", "as": "worker_a" },
                { "of": "KnowledgeGraphManager", "as": "worker_b" }
            ]
        }
    }"#;

    let spec = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript")
        .expect("extraction should succeed");

    let worker_a = &spec.model_config.automata[0];
    let worker_b = &spec.model_config.automata[1];

    let a_labels: std::collections::HashSet<&str> = worker_a
        .transitions
        .iter()
        .map(|t| t.label.as_str())
        .collect();
    let b_labels: std::collections::HashSet<&str> = worker_b
        .transitions
        .iter()
        .map(|t| t.label.as_str())
        .collect();

    // No labels in common — zero alphabet intersection.
    let intersection: std::collections::HashSet<&&str> = a_labels.intersection(&b_labels).collect();
    assert!(
        intersection.is_empty(),
        "expected disjoint label sets between worker_a and worker_b, got intersection: {:?}",
        intersection
    );
}

/// Error case: instance references a class not present in `targets[]`.
/// The extractor must surface a clear, actionable error message rather
/// than silently producing an empty automaton.
#[test]
fn compositional_extract_unknown_class_errors() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load"] }
            }
        ],
        "composition": {
            "type": "asynchronous",
            "name": "race",
            "instances": [
                { "of": "KnowledgeGraphManager", "as": "worker_a" },
                { "of": "MissingClass", "as": "ghost" }
            ]
        }
    }"#;

    let result = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript");
    let err = result.expect_err("expected error for unknown class");
    assert!(
        err.contains("MissingClass"),
        "error should mention the missing class name, got: {}",
        err
    );
    assert!(
        err.contains("no matching target") || err.contains("not found"),
        "error should explain why, got: {}",
        err
    );
}

/// GAP-008 — composition.resources[] produces a hand-modeled automaton
/// alongside the per-instance ones. The resource's transitions and states
/// flow through the espec output unchanged; the resource's labels are
/// NOT per-instance-prefixed (they're authored verbatim and synchronize
/// via alphabet intersection).
#[test]
fn compositional_extract_with_hand_modeled_resource() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load", "save"] }
            }
        ],
        "composition": {
            "type": "asynchronous",
            "name": "memory_write_race",
            "instances": [
                { "of": "KnowledgeGraphManager", "as": "worker_a" },
                { "of": "KnowledgeGraphManager", "as": "worker_b" }
            ],
            "shared": ["ev_save"],
            "resources": [
                {
                    "name": "shared_file",
                    "states": ["v0", "v1", "clobbered"],
                    "initial": "v0",
                    "transitions": [
                        { "from": "v0", "to": "v1", "label": "ev_save" },
                        { "from": "v1", "to": "clobbered", "label": "ev_save" }
                    ]
                }
            ]
        }
    }"#;

    let spec = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript")
        .expect("extraction with resource should succeed");

    // Three automata: 2 instances + 1 resource.
    assert_eq!(
        spec.model_config.automata.len(),
        3,
        "expected 3 automata (2 instances + 1 resource), got: {:?}",
        spec.model_config
            .automata
            .iter()
            .map(|a| &a.id)
            .collect::<Vec<_>>()
    );
    let ids: Vec<&str> = spec
        .model_config
        .automata
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(ids, vec!["worker_a", "worker_b", "shared_file"]);

    // The resource has the declared 3 states.
    let shared_file = spec
        .model_config
        .automata
        .iter()
        .find(|a| a.id == "shared_file")
        .expect("shared_file automaton should be emitted");
    assert_eq!(shared_file.states.len(), 3);
    // Initial state is v0.
    let initials: Vec<&str> = shared_file
        .states
        .iter()
        .filter_map(|s| match s {
            mununu_core::adapter::extraction::ast::StateDef::Structured(sd) => {
                if sd.initial {
                    Some(sd.name.as_str())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(initials, vec!["v0"]);

    // Resource transitions are emitted verbatim — labels NOT prefixed.
    let labels: Vec<&str> = shared_file
        .transitions
        .iter()
        .map(|t| t.label.as_str())
        .collect();
    assert_eq!(labels, vec!["ev_save", "ev_save"]);

    // Composition.members lists instances + resource in order.
    let comp = spec.model_config.composition.as_ref().unwrap();
    assert_eq!(comp.members, vec!["worker_a", "worker_b", "shared_file"]);

    // Now the alphabet intersection: both worker instances should carry
    // the shared `ev_save` label verbatim (because it's in
    // `composition.shared`), AND the resource carries the same label.
    // That's the three-way synchronization point.
    for instance_id in ["worker_a", "worker_b"] {
        let inst = spec
            .model_config
            .automata
            .iter()
            .find(|a| a.id == instance_id)
            .unwrap();
        let inst_labels: Vec<&str> = inst.transitions.iter().map(|t| t.label.as_str()).collect();
        assert!(
            inst_labels.contains(&"ev_save"),
            "{instance_id} should carry shared `ev_save` label verbatim, got: {:?}",
            inst_labels
        );
    }
}

/// GAP-008 — `initial` not in `states` produces a clear error.
#[test]
fn compositional_resource_initial_not_in_states_errors() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load"] }
            }
        ],
        "composition": {
            "type": "asynchronous",
            "name": "race",
            "instances": [
                { "of": "KnowledgeGraphManager", "as": "worker_a" }
            ],
            "resources": [
                {
                    "name": "shared_file",
                    "states": ["v0", "v1"],
                    "initial": "v9",
                    "transitions": []
                }
            ]
        }
    }"#;

    let err = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript")
        .expect_err("expected error for invalid initial");
    assert!(
        err.contains("initial = 'v9'"),
        "error should mention the bad initial state, got: {err}"
    );
    assert!(err.contains("not in `states`"));
}

/// GAP-008 — transition referencing a non-declared state produces a clear error.
#[test]
fn compositional_resource_transition_unknown_state_errors() {
    let config = r#"{
        "$schema": "extraction_config_v1",
        "domain": "mcp_server",
        "language": "typescript",
        "source": { "file": "test.ts" },
        "targets": [
            {
                "class": "KnowledgeGraphManager",
                "state_fields": ["state"],
                "methods": { "include": ["load"] }
            }
        ],
        "composition": {
            "type": "asynchronous",
            "name": "race",
            "instances": [
                { "of": "KnowledgeGraphManager", "as": "worker_a" }
            ],
            "resources": [
                {
                    "name": "shared_file",
                    "states": ["v0", "v1"],
                    "initial": "v0",
                    "transitions": [
                        { "from": "v0", "to": "phantom", "label": "save" }
                    ]
                }
            ]
        }
    }"#;

    let err = extract_from_source(config, MINIMAL_MCP005_SOURCE, "typescript")
        .expect_err("expected error for unknown transition target");
    assert!(
        err.contains("phantom"),
        "error should mention the bad state name, got: {err}"
    );
    assert!(err.contains("not in `states`"));
}
