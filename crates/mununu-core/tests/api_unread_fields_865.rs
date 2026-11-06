//! mununu#865 — request fields the HTTP API accepted and never read (siblings of #862): each one
//! changed nothing, so a client that set it got an answer to a question it did not ask. Each test
//! below sets the field and checks that the route now acts on it or refuses it with a 400 naming
//! it.
//!
//! Its own top-level integration test (target `api_unread_fields_865`), outside `handlers.rs`
//! where the fixes live, so `make verify-repro FEATURES=api` builds the parent without them; it
//! calls only handlers the parent already has.

use axum::Json;
use mununu_core::api::error::ApiError;
use mununu_core::api::handlers::{
    btor2_cegar_handler, btor2_verify_liveness_handler, context_import_handler,
    context_summarize_handler, context_synthesize_handler,
};
use serde_json::{Value, json};

/// An uncontrollable `fail` leads into `Bad`, so "never `Bad`" is unrealizable: the environment
/// wins, and the route can compute its counterstrategy.
const UNREALIZABLE: &str = r#"
context trap {
    alphabet { label go; label fail; }
    automata {
        automaton T {
            controllable { label go; }
            states { state Ok initial; state Bad; }
            transitions {
                transition Ok -> Ok on label go;
                transition Ok -> Bad on label fail;
                transition Bad -> Bad on label go;
            }
            predicates { predicate is_bad = state Bad; }
        }
    }
    mu_formulas {
        formula never_bad {
            over T;
            body = nu X. ((!is_bad) && ([] X));
        }
    }
}
"#;

/// A 1-bit register that stays 0, as BTOR2.
const BTOR2_SAFE: &str =
    "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 next 1 3 2\n6 bad 3\n";

fn from<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).expect("a well-formed request")
}

fn refused_naming<T: std::fmt::Debug>(r: Result<T, ApiError>, field: &str) {
    match r {
        Err(ApiError::BadRequest { message, .. }) => assert!(
            message.contains(field),
            "{field}: the refusal names it: {message}"
        ),
        Err(other) => panic!("{field}: refused for another reason: {other}"),
        Ok(v) => panic!("{field}: accepted and ignored: {v:?}"),
    }
}

mod api_unread_fields_865 {
    use super::*;

    #[tokio::test]
    async fn summarize_refuses_the_table_format_it_never_produced() {
        let r = context_summarize_handler(Json(from(json!({
            "context": { "name": "trap.ctxdsl", "content": UNREALIZABLE },
            "format": "table",
        }))))
        .await;
        refused_naming(r, "table");
    }

    #[tokio::test]
    async fn synthesis_skips_the_counterstrategy_when_the_request_switches_it_off() {
        let ask = |counterstrategy: bool| {
            from(json!({
                "context": { "name": "trap.ctxdsl", "content": UNREALIZABLE },
                "formula": "never_bad",
                "automaton": "T",
                "options": { "diagnostics": { "counterstrategy": counterstrategy } },
            }))
        };
        let on = context_synthesize_handler(Json(ask(true)))
            .await
            .expect("synthesis runs");
        assert!(
            !on.0.realizable,
            "the model is unrealizable by construction"
        );
        assert!(on.0.counterstrategy.is_some(), "on: computed");
        let off = context_synthesize_handler(Json(ask(false)))
            .await
            .expect("synthesis runs");
        assert!(
            off.0.counterstrategy.is_none(),
            "off: the switch is applied, not ignored"
        );
    }

    #[tokio::test]
    async fn import_under_auto_lets_the_filename_extension_pick_the_adapter() {
        // TLSF content named `.btor2`: content sniffing alone takes the TLSF adapter and
        // succeeds; with the filename read, the extension picks BTOR2 (as the CLI's loader
        // does), and the BTOR2 parser rejects the TLSF text.
        let tlsf = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/tlsf/request_grant.tlsf"
        ))
        .expect("the fixture");
        let r = context_import_handler(Json(from(json!({
            "content": tlsf,
            "format": "auto",
            "filename": "request_grant.btor2",
        }))))
        .await;
        assert!(
            r.is_err(),
            "the filename decides the adapter under `auto`: {:?}",
            r.ok().map(|j| j.0.success)
        );
    }

    #[tokio::test]
    async fn liveness_refuses_grant_beside_responses() {
        let r = btor2_verify_liveness_handler(Json(from(json!({
            "content": BTOR2_SAFE,
            "responses": ["q -> q"],
            "grant": "q",
        }))))
        .await;
        refused_naming(r, "grant");
    }

    #[tokio::test]
    async fn cegar_refuses_an_unknown_must_edge_policy() {
        let r = btor2_cegar_handler(Json(from(json!({
            "content": BTOR2_SAFE,
            "formula": "nu X. ((q == 0) && ([] X))",
            "predicates": [{ "name": "q0", "register": "q", "value": 0 }],
            "must_edge_inference": "sampling-confluence",
        }))))
        .await;
        refused_naming(r, "must_edge_inference");
    }

    #[tokio::test]
    async fn symbolic_cegar_refuses_the_explicit_lift_options_it_does_not_read() {
        let r = btor2_cegar_handler(Json(from(json!({
            "content": BTOR2_SAFE,
            "formula": "nu X. ((q == 0) && ([] X))",
            "predicates": [{ "name": "q0", "register": "q", "value": 0 }],
            "engine": "symbolic",
            // The UI's defaults ride along on every run and are accepted.
            "predicate_source": "wp",
            "must_edge_inference": "off",
            "may_edge_inference": "off",
            "emit_ctxdsl": true,
        }))))
        .await;
        refused_naming(r, "emit_ctxdsl");
    }
}
