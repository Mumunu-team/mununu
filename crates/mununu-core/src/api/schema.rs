//! JSON Schema derivation for the API wire types (mununu#476 item 5 follow-up).
//!
//! Every type serialised as part of a `POST /api/v1/*` response body carries
//! `#[derive(schemars::JsonSchema)]`. This module exposes the generated
//! schemas as `serde_json::Value` so the drift-detector test in
//! `tests/api_schema_drift.rs` can regenerate them and diff against the
//! committed files under [`docs/api-schemas/`].
//!
//! **Consumer flow:**
//!
//! 1. Fetch the shipped schema file — e.g.
//!    `docs/api-schemas/sv-verify-auto-response.schema.json` — pinned to a
//!    specific mununu release.
//! 2. Point your codegen at it (`quicktype`, `datamodel-code-generator`,
//!    `openapi-typescript`, or the JSON-Schema-native tool for your language).
//! 3. On a mununu binary bump, either accept the drift (the schema in that
//!    release is the new contract) or reject it (pin the previous release
//!    while you migrate). The consumer briefing that ships with any wire-
//!    format change explains what moved.
//!
//! **Why not OpenAPI (via `utoipa`)?** For now, downstream consumers want
//! machine-readable per-response schemas, not a full `/openapi.json` endpoint
//! with axum handler integration. `schemars` is the lighter dependency and
//! its output composes cleanly with every JSON-Schema-native codegen. A
//! future `utoipa` layer can co-exist — it just consumes the same derived
//! `JsonSchema` impls.

use schemars::schema_for;

use super::models::{
    Btor2CheckFsmRequest, Btor2CheckFsmResponse, Btor2VerifyLivenessAllRequest,
    Btor2VerifyLivenessRequest, Btor2VerifyLivenessResponse,
    Btor2VerifyLivenessUnderFairnessRequest, Btor2VerifyRecoverabilityRequest,
    Btor2VerifyRecoverabilityResponse, Btor2VerifyRequest, Btor2VerifyResponse, SvCheckFsmRequest,
    SvVerifyAutoRequest, SvVerifyAutoResponse, SvVerifyLivenessAllRequest, SvVerifyLivenessRequest,
    SvVerifyRecoverabilityRequest, SvVerifyRequest,
};

/// JSON Schema (Draft 2019-09) for `POST /api/v1/sv/verify-auto` request bodies.
pub fn sv_verify_auto_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvVerifyAutoRequest))
        .expect("SvVerifyAutoRequest schema must serialise as JSON")
}

/// JSON Schema (Draft 2019-09) for `POST /api/v1/sv/verify-auto` responses.
pub fn sv_verify_auto_response_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvVerifyAutoResponse))
        .expect("SvVerifyAutoResponse schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify` request bodies.
pub fn btor2_verify_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyRequest))
        .expect("Btor2VerifyRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify` responses. Also the response shape
/// for `POST /api/v1/sv/verify` (SV → BTOR2 lift then decide).
pub fn btor2_verify_response_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyResponse))
        .expect("Btor2VerifyResponse schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify-liveness` request bodies.
pub fn btor2_verify_liveness_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyLivenessRequest))
        .expect("Btor2VerifyLivenessRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify-liveness` responses. Also the response
/// shape for `/btor2/verify-liveness-all`, `/sv/verify-liveness`, and
/// `/sv/verify-liveness-all`.
pub fn btor2_verify_liveness_response_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyLivenessResponse))
        .expect("Btor2VerifyLivenessResponse schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify-liveness-all` request bodies.
pub fn btor2_verify_liveness_all_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyLivenessAllRequest))
        .expect("Btor2VerifyLivenessAllRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify-liveness-under-fairness` request
/// bodies (mununu#477 Option B). Response shape is the shared
/// [`Btor2VerifyLivenessResponse`].
pub fn btor2_verify_liveness_under_fairness_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyLivenessUnderFairnessRequest))
        .expect("Btor2VerifyLivenessUnderFairnessRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify-recoverability` request bodies.
pub fn btor2_verify_recoverability_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyRecoverabilityRequest))
        .expect("Btor2VerifyRecoverabilityRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/verify-recoverability` responses. Also the
/// response shape for `POST /api/v1/sv/verify-recoverability`. Includes the
/// optional structured [`crate::verdict::VerdictRefinement`] tree.
pub fn btor2_verify_recoverability_response_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2VerifyRecoverabilityResponse))
        .expect("Btor2VerifyRecoverabilityResponse schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/check-fsm` request bodies.
pub fn btor2_check_fsm_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2CheckFsmRequest))
        .expect("Btor2CheckFsmRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/btor2/check-fsm` responses. Also the response
/// shape for `POST /api/v1/sv/check-fsm`.
pub fn btor2_check_fsm_response_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Btor2CheckFsmResponse))
        .expect("Btor2CheckFsmResponse schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/sv/verify` request bodies (SV lift + BTOR2 verify).
pub fn sv_verify_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvVerifyRequest))
        .expect("SvVerifyRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/sv/verify-liveness` request bodies.
pub fn sv_verify_liveness_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvVerifyLivenessRequest))
        .expect("SvVerifyLivenessRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/sv/verify-liveness-all` request bodies.
pub fn sv_verify_liveness_all_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvVerifyLivenessAllRequest))
        .expect("SvVerifyLivenessAllRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/sv/verify-recoverability` request bodies.
pub fn sv_verify_recoverability_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvVerifyRecoverabilityRequest))
        .expect("SvVerifyRecoverabilityRequest schema must serialise as JSON")
}

/// JSON Schema for `POST /api/v1/sv/check-fsm` request bodies.
pub fn sv_check_fsm_request_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(SvCheckFsmRequest))
        .expect("SvCheckFsmRequest schema must serialise as JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: schema generation produces a top-level object with `properties`,
    /// `$schema`, and a `title`. Guards against a schemars breaking change.
    #[test]
    fn request_schema_has_expected_top_level_shape() {
        let s = sv_verify_auto_request_schema();
        assert!(
            s.get("$schema").is_some(),
            "schema must carry `$schema`: {s}"
        );
        assert!(s.get("title").is_some(), "schema must carry `title`: {s}");
        assert_eq!(
            s.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "request schema must be an object at the root: {s}"
        );
    }

    #[test]
    fn response_schema_has_expected_top_level_shape() {
        let s = sv_verify_auto_response_schema();
        assert!(s.get("$schema").is_some());
        assert!(s.get("title").is_some());
        assert_eq!(s.get("type").and_then(|v| v.as_str()), Some("object"));
    }

    /// The response schema must reference every wire type documented in
    /// `docs/api-schemas/verdict.md` — `PropertyVerdictView`, `CounterexampleView`,
    /// mununu#536 — a REAL report, serialized through the one shared conversion, must conform
    /// to the published response schema.
    ///
    /// The other drift tests compare the schema against the Rust types. This one compares an
    /// actual serialized document against the schema, which is the thing a consumer holds. It
    /// is the test that makes the CLI's `--json` and the HTTP response unable to diverge
    /// again: both call `SvVerifyAutoResponse::from(&report)`, so conformance here is
    /// conformance for both surfaces.
    ///
    /// Checks in both directions — every `required` key is present, and every emitted key is
    /// declared. A one-directional check would miss the failure that actually happened: a
    /// serializer emitting a field the schema never mentioned.
    #[test]
    fn a_serialized_report_conforms_to_the_published_response_schema() {
        use crate::adapter::slang::translate::SvaKind;
        use crate::adapter::slang::verify_auto::{
            AutoVerifyReport, ExactCounterexample, ModelDiagnostics, NoteLevel, PropertyVerdict,
            VerificationNote, VerifyOutcome,
        };

        // Exercise every outcome arm and every optional field, so the check is not vacuous on
        // a report that happens to omit the interesting parts.
        let report = AutoVerifyReport {
            properties: vec![
                PropertyVerdict {
                    name: "p_holds".into(),
                    label: Some("a_holds_forever".into()),
                    kind: SvaKind::Assert,
                    formula: "nu X. (a && [] X)".into(),
                    outcome: VerifyOutcome::Holds,
                    seeded_predicates: vec!["a == 1".into()],
                    counterexample: None,
                },
                PropertyVerdict {
                    name: "p_violated".into(),
                    label: Some("c_cover_b".into()),
                    kind: SvaKind::Cover,
                    formula: "mu X. (b || <> X)".into(),
                    outcome: VerifyOutcome::Violated { false_cells: 3 },
                    seeded_predicates: vec![],
                    counterexample: Some(ExactCounterexample {
                        prefix: vec![vec![("q".into(), 0)]],
                        cycle: vec![vec![("q".into(), 1)]],
                        inputs: vec![vec![("d".into(), 1)]],
                        unreachable_target: vec!["b".into()],
                    }),
                },
                PropertyVerdict {
                    name: "p_unknown".into(),
                    label: None,
                    kind: SvaKind::Assume,
                    formula: "nu X. (c && [] X)".into(),
                    outcome: VerifyOutcome::Unknown { unknown_cells: 32 },
                    seeded_predicates: vec![],
                    counterexample: None,
                },
                PropertyVerdict {
                    name: "p_skipped".into(),
                    label: Some("a_wide_cone".into()),
                    kind: SvaKind::Assert,
                    formula: "nu X. (d && [] X)".into(),
                    outcome: VerifyOutcome::Skipped {
                        reason: "bit cap".into(),
                    },
                    seeded_predicates: vec![],
                    counterexample: None,
                },
            ],
            unsupported: vec![("u0".into(), "unsupported binary op: BinaryAnd".into())],
            diagnostics: ModelDiagnostics {
                state_register_count: 2,
                blackboxed_modules: vec!["m".into()],
                gated_resets: vec!["rst_n=1".into()],
                auto_provided_stubs: vec![],
                frontend: Some("read_verilog + sv2v".into()),
                frontend_fallback_reason: Some("slang errored".into()),
                applied_config_values: vec!["cfg=7".into()],
                cutpoint_signals: vec!["fits".into()],
            },
            notes: vec![VerificationNote {
                kind: "coverage-summary".into(),
                level: NoteLevel::Info,
                summary: "4 assertion(s)".into(),
                detail: "d".into(),
                items: vec!["i".into()],
            }],
        };

        let doc = serde_json::to_value(SvVerifyAutoResponse::from(&report))
            .expect("the response serialises");
        let schema =
            serde_json::to_value(sv_verify_auto_response_schema()).expect("schema serialises");

        // Walk one object against one schema node: required present, emitted declared.
        fn conforms(
            doc: &serde_json::Value,
            node: &serde_json::Value,
            schema: &serde_json::Value,
            path: &str,
        ) {
            // Follow a `$ref` / `allOf` indirection to the definition it names.
            let node = resolve(node, schema);
            let Some(props) = node.get("properties").and_then(|p| p.as_object()) else {
                return;
            };
            let obj = doc
                .as_object()
                .unwrap_or_else(|| panic!("{path}: expected an object"));
            if let Some(req) = node.get("required").and_then(|r| r.as_array()) {
                for r in req {
                    let k = r.as_str().unwrap_or_default();
                    assert!(
                        obj.contains_key(k),
                        "{path}.{k} is `required` by the schema but absent from the serialized \
                         document — a consumer following the schema would break"
                    );
                }
            }
            for (k, v) in obj {
                let Some(child) = props.get(k) else {
                    panic!(
                        "{path}.{k} is emitted but NOT declared in the schema — this is exactly \
                         the drift the shared conversion exists to prevent"
                    );
                };
                match v {
                    serde_json::Value::Object(_) => {
                        conforms(v, child, schema, &format!("{path}.{k}"))
                    }
                    serde_json::Value::Array(items) => {
                        let inner = resolve(child, schema);
                        if let Some(item_schema) = inner.get("items") {
                            for (i, it) in items.iter().enumerate() {
                                if it.is_object() || it.is_array() {
                                    conforms(it, item_schema, schema, &format!("{path}.{k}[{i}]"));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // `$ref: "#/definitions/X"`, or an `allOf: [{ $ref }]` wrapper (how schemars emits a
        // named type behind an optional field).
        fn resolve<'a>(
            node: &'a serde_json::Value,
            schema: &'a serde_json::Value,
        ) -> &'a serde_json::Value {
            if let Some(r) = node.get("$ref").and_then(|r| r.as_str())
                && let Some(name) = r.strip_prefix("#/definitions/")
                && let Some(def) = schema.get("definitions").and_then(|d| d.get(name))
            {
                return resolve(def, schema);
            }
            if let Some(all) = node.get("allOf").and_then(|a| a.as_array())
                && let Some(first) = all.first()
            {
                return resolve(first, schema);
            }
            node
        }

        conforms(&doc, &schema, &schema, "$");

        // Non-vacuity: the walk must actually have reached the fields mununu#536 added, or a
        // conformance pass proves nothing about them.
        assert_eq!(
            doc["properties"][2]["unknown_cells"], 32,
            "the ⊥ figure is machine-readable"
        );
        assert_eq!(doc["properties"][1]["false_cells"], 3);
        assert_eq!(doc["properties"][3]["skip_reason"], "bit cap");
        assert_eq!(doc["diagnostics"]["frontend"], "read_verilog + sv2v");
        assert_eq!(doc["diagnostics"]["config_values"][0], "cfg=7");
        assert_eq!(doc["diagnostics"]["cutpoints"][0], "fits");
        assert_eq!(
            doc["properties"][1]["counterexample"]["inputs"][0][0]["register"],
            "d"
        );
        // `holds` carries no cell count, and the optional fields are OMITTED rather than null —
        // a consumer checking `"unknown_cells" in p` must not see a null.
        assert!(doc["properties"][0].get("unknown_cells").is_none());
        assert!(doc["properties"][0].get("false_cells").is_none());
    }

    /// `CexCellView`, `VerificationNoteView`, `ModelDiagnosticsView`,
    /// `UnsupportedAssertionView`. Guards against silent removal of a
    /// documented type from the schema tree.
    #[test]
    fn response_schema_references_documented_types() {
        let s = sv_verify_auto_response_schema();
        let s_str = serde_json::to_string(&s).expect("schema serialises");
        for name in [
            "PropertyVerdictView",
            "CounterexampleView",
            "CexCellView",
            "VerificationNoteView",
            "ModelDiagnosticsView",
            "UnsupportedAssertionView",
        ] {
            assert!(
                s_str.contains(name),
                "response schema is missing `{name}` — a documented wire type was dropped from the tree"
            );
        }
    }

    /// Drift-detector — the committed schema files under `docs/api-schemas/`
    /// must match the schema derived from the current Rust types. When they
    /// disagree, the wire format changed and the committed contract is stale.
    ///
    /// **To update the committed schemas** after a wire-format change:
    ///
    /// ```sh
    /// MUNUNU_UPDATE_API_SCHEMAS=1 \
    ///   cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1
    /// ```
    ///
    /// This writes the derived schemas to disk; a follow-up PR commits them
    /// alongside a consumer briefing per `docs/policies/cross-repo-impact.md`.
    fn drift_check(rel_path: &str, derived: &serde_json::Value) {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let committed_path = repo_root.join(rel_path);
        let derived_pretty =
            serde_json::to_string_pretty(derived).expect("pretty-print derived schema");
        if std::env::var("MUNUNU_UPDATE_API_SCHEMAS").is_ok() {
            if let Some(parent) = committed_path.parent() {
                std::fs::create_dir_all(parent).expect("create schema-doc dir");
            }
            let mut with_trailing_newline = derived_pretty.clone();
            with_trailing_newline.push('\n');
            std::fs::write(&committed_path, with_trailing_newline).expect("write derived schema");
            return;
        }
        let committed = std::fs::read_to_string(&committed_path).unwrap_or_else(|e| {
            panic!(
                "the committed schema file `{}` is missing ({e}). Run \
                 `MUNUNU_UPDATE_API_SCHEMAS=1 cargo test -p mununu-core --lib --features api api_schema_drift_` \
                 to generate it, then commit the result.",
                rel_path
            )
        });
        let committed_json: serde_json::Value =
            serde_json::from_str(&committed).expect("committed schema parses as JSON");
        assert_eq!(
            derived, &committed_json,
            "derived schema disagrees with committed `{rel_path}` — the wire format changed.\n\
             Update the committed file: MUNUNU_UPDATE_API_SCHEMAS=1 \
             cargo test -p mununu-core --lib --features api api_schema_drift_ \n\
             Then ship a consumer briefing per docs/policies/cross-repo-impact.md.",
        );
    }

    #[test]
    fn api_schema_drift_sv_verify_auto_request() {
        drift_check(
            "docs/api-schemas/sv-verify-auto-request.schema.json",
            &sv_verify_auto_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_sv_verify_auto_response() {
        drift_check(
            "docs/api-schemas/sv-verify-auto-response.schema.json",
            &sv_verify_auto_response_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_request() {
        drift_check(
            "docs/api-schemas/btor2-verify-request.schema.json",
            &btor2_verify_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_response() {
        drift_check(
            "docs/api-schemas/btor2-verify-response.schema.json",
            &btor2_verify_response_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_liveness_request() {
        drift_check(
            "docs/api-schemas/btor2-verify-liveness-request.schema.json",
            &btor2_verify_liveness_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_liveness_response() {
        drift_check(
            "docs/api-schemas/btor2-verify-liveness-response.schema.json",
            &btor2_verify_liveness_response_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_liveness_all_request() {
        drift_check(
            "docs/api-schemas/btor2-verify-liveness-all-request.schema.json",
            &btor2_verify_liveness_all_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_liveness_under_fairness_request() {
        drift_check(
            "docs/api-schemas/btor2-verify-liveness-under-fairness-request.schema.json",
            &btor2_verify_liveness_under_fairness_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_recoverability_request() {
        drift_check(
            "docs/api-schemas/btor2-verify-recoverability-request.schema.json",
            &btor2_verify_recoverability_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_verify_recoverability_response() {
        drift_check(
            "docs/api-schemas/btor2-verify-recoverability-response.schema.json",
            &btor2_verify_recoverability_response_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_check_fsm_request() {
        drift_check(
            "docs/api-schemas/btor2-check-fsm-request.schema.json",
            &btor2_check_fsm_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_btor2_check_fsm_response() {
        drift_check(
            "docs/api-schemas/btor2-check-fsm-response.schema.json",
            &btor2_check_fsm_response_schema(),
        );
    }

    #[test]
    fn api_schema_drift_sv_verify_request() {
        drift_check(
            "docs/api-schemas/sv-verify-request.schema.json",
            &sv_verify_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_sv_verify_liveness_request() {
        drift_check(
            "docs/api-schemas/sv-verify-liveness-request.schema.json",
            &sv_verify_liveness_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_sv_verify_liveness_all_request() {
        drift_check(
            "docs/api-schemas/sv-verify-liveness-all-request.schema.json",
            &sv_verify_liveness_all_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_sv_verify_recoverability_request() {
        drift_check(
            "docs/api-schemas/sv-verify-recoverability-request.schema.json",
            &sv_verify_recoverability_request_schema(),
        );
    }

    #[test]
    fn api_schema_drift_sv_check_fsm_request() {
        drift_check(
            "docs/api-schemas/sv-check-fsm-request.schema.json",
            &sv_check_fsm_request_schema(),
        );
    }
}
