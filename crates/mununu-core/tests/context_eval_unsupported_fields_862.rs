//! mununu#862 — `POST /api/v1/context/eval` declared `hide`, `minimize` and `stubs` and never read
//! them: a request that set one succeeded, and its verdicts were computed on the unhidden,
//! unminimised, unstubbed model with nothing in the response saying so. The route now refuses a
//! set one with a 400 that names it.
//!
//! Its own top-level integration test (target `context_eval_unsupported_fields_862`), outside
//! `handlers.rs` where the fix lives, so `make verify-repro FEATURES=api` builds the parent
//! without the fix; it calls only the handler the parent already has.

use axum::Json;
use mununu_core::api::error::ApiError;
use mununu_core::api::handlers::context_verify_handler;
use mununu_core::api::models::ContextVerifyRequest;
use serde_json::{Value, json};

const TOGGLE: &str = r#"
context toggle {
    alphabet { label a; label b; }
    automata {
        automaton T {
            states { state S0 initial; state S1; }
            transitions {
                transition S0 -> S1 on label a;
                transition S1 -> S0 on label b;
            }
        }
    }
    mu_formulas {
        formula can_fire_a {
            over T;
            body = <a> true;
        }
    }
}
"#;

fn request(extra: Value) -> ContextVerifyRequest {
    let mut body = json!({
        "context": { "name": "toggle.ctxdsl", "content": TOGGLE },
        "formula": "can_fire_a",
        "automaton": "T",
    });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    serde_json::from_value(body).expect("a well-formed request")
}

mod context_eval_unsupported_fields_862 {
    use super::*;

    /// The positive control: the same request with none of the three set is evaluated.
    #[tokio::test]
    async fn a_request_without_the_unapplied_fields_is_evaluated() {
        let response = context_verify_handler(Json(request(json!({})))).await;
        assert!(response.is_ok(), "{:?}", response.err());
    }

    #[tokio::test]
    async fn context_eval_refuses_hide_minimize_and_stubs_instead_of_ignoring_them() {
        for (field, extra) in [
            ("hide", json!({ "hide": ["a"] })),
            ("minimize", json!({ "minimize": true })),
            (
                "stubs",
                json!({ "stubs": [{ "name": "s.espec.json", "content": "{}" }] }),
            ),
        ] {
            match context_verify_handler(Json(request(extra))).await {
                Err(ApiError::BadRequest { message, .. }) => assert!(
                    message.contains(field),
                    "{field}: the refusal names the field: {message}"
                ),
                Err(other) => panic!("{field}: refused for another reason: {other}"),
                Ok(_) => panic!("{field}: accepted and ignored — the verdict is on another model"),
            }
        }
    }
}
