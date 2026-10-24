//! HTTP server setup and configuration.
//!
//! This module configures the Axum HTTP server with routing, CORS, and error handling.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{
    cors::{Any, CorsLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::api::handlers;

/// Start the HTTP server
pub async fn start_server(
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = create_router();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("Server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Create the API router
fn create_router() -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/v1/health", get(handlers::health_check))
        .route(
            "/api/v1/context/summarize",
            post(handlers::context_summarize_handler),
        )
        .route(
            "/api/v1/context/synth",
            post(handlers::context_synthesize_handler),
        )
        .route(
            "/api/v1/context/graph",
            post(handlers::context_graphs_handler),
        )
        .route(
            "/api/v1/context/eval",
            post(handlers::context_verify_handler),
        )
        .route(
            "/api/v1/context/import",
            post(handlers::context_import_handler),
        )
        .route(
            "/api/v1/extraction/domains",
            get(handlers::extraction_domains_handler),
        )
        .route(
            "/api/v1/extraction/composition-modes",
            get(handlers::extraction_composition_modes_handler),
        )
        .route(
            "/api/v1/extraction/propose-composition",
            post(handlers::extraction_propose_composition_handler),
        )
        .route(
            "/api/v1/extraction/extract",
            post(handlers::extraction_extract_handler),
        )
        .route(
            "/api/v1/extraction/validate",
            post(handlers::extraction_validate_handler),
        )
        .route(
            "/api/v1/context/predicates",
            post(handlers::context_predicates_handler),
        )
        .route("/api/v1/templates", get(handlers::templates_handler))
        .route(
            "/api/v1/contract/validate",
            post(handlers::contract_validate_handler),
        )
        .route(
            "/api/v1/contract/discover",
            post(handlers::contract_discover_handler),
        )
        .route(
            "/api/v1/contract/query",
            post(handlers::contract_query_handler),
        )
        .route(
            "/api/v1/contract/review",
            post(handlers::contract_review_handler),
        )
        .route(
            "/api/v1/codesign/verify",
            post(handlers::codesign_verify_handler),
        )
        .route("/api/v1/verify", post(handlers::verify_project_handler))
        .route("/api/v1/btor2/cegar", post(handlers::btor2_cegar_handler))
        // Multi-engine safety portfolio — decide `bad`-reachability across every
        // sound engine (exact ⊕ native ⊕ spacer ⊕ btormc ⊕ Pono), merged under the
        // differential-oracle discipline. Surface peer of the CLI `mununu btor2 verify`.
        .route("/api/v1/btor2/verify", post(handlers::btor2_verify_handler))
        // P2 response-liveness at scale — AG(request → AF grant) via liveness-to-
        // safety + the portfolio. Surface peer of the CLI `mununu btor2 verify-liveness`.
        .route(
            "/api/v1/btor2/verify-liveness",
            post(handlers::btor2_verify_liveness_handler),
        )
        // P2 recoverability — AG EF good ("can it always get back?"), the branching
        // property SVA cannot state. Surface peer of `mununu btor2 verify-recoverability`.
        .route(
            "/api/v1/btor2/verify-recoverability",
            post(handlers::btor2_verify_recoverability_handler),
        )
        // P1 auto FSM-recoverability scan — discover FSM-like state registers and
        // decide recoverability of each to its idle/reset value with no user input.
        // Surface peer of the CLI `mununu btor2 check-fsm`.
        .route(
            "/api/v1/btor2/check-fsm",
            post(handlers::btor2_check_fsm_handler),
        )
        // P2.5-F two-player game — decide controllable-reachability of `good` under an input partition
        // (controller vs environment) and synthesize the winner's strategy (controller Mealy strategy or
        // environment counterstrategy). Surface peer of the CLI `mununu btor2 game`.
        .route("/api/v1/btor2/game", post(handlers::btor2_game_handler))
        // cegar-extraction Stage 2 — SV-direct CEGAR one-call (sv2v +
        // Yosys → flattened BTOR2 → cegar_refine_loop). Surface peer of
        // the CLI `mununu sv cegar`; lets the extraction-tab SV workflow
        // run CEGAR without a manual emit-btor2-per-module step.
        .route("/api/v1/sv/cegar", post(handlers::sv_cegar_handler))
        // Track-H SVA front-end (XL.6a) — slang extracts + translates the
        // design's SVA to mu-calculus. Surface peer of `mununu sv extract-sva`
        // and the extraction-tab SVA panel. No verification (that is verify-auto).
        .route(
            "/api/v1/sv/extract-sva",
            post(handlers::sv_extract_sva_handler),
        )
        // Track-H no-sidecar verify (XL.6b) — extract SVA + verify each property
        // against the model. Surface peer of `mununu sv verify-auto` + the UI panel.
        .route(
            "/api/v1/sv/verify-auto",
            post(handlers::sv_verify_auto_handler),
        )
        // SV-direct verbs — lift SV (sv2v + Yosys) then decide a property in one call
        // (safety / response-liveness / recoverability). Surface peers of the CLI
        // `sv verify` / `verify-liveness` / `verify-recoverability`.
        .route("/api/v1/sv/verify-bad", post(handlers::sv_verify_handler))
        .route(
            "/api/v1/sv/verify-liveness",
            post(handlers::sv_verify_liveness_handler),
        )
        .route(
            "/api/v1/sv/verify-recoverability",
            post(handlers::sv_verify_recoverability_handler),
        )
        // SV-direct auto FSM illegal-encoding scan (lift + scan, one call). Surface peer
        // of the CLI `sv check-fsm` and the BTOR2-direct `/api/v1/btor2/check-fsm`.
        .route("/api/v1/sv/check-fsm", post(handlers::sv_check_fsm_handler))
        // SV-direct partial-write preflight (monono#partsel) — lift + report the
        // registers the verifier can't keep faithfully, no model checking. Surface
        // peer of the CLI `sv lint`.
        .route("/api/v1/sv/lint", post(handlers::sv_lint_handler))
        .route("/api/v1/sv/cdc-lint", post(handlers::sv_cdc_lint_handler))
        // SV-direct mutation (#468) — apply a named structural fault + re-verify to
        // check the property verdicts flip (property adequacy), or list targets.
        // Surface peer of the CLI `sv mutate`.
        .route("/api/v1/sv/mutate", post(handlers::sv_mutate_handler))
        .route("/api/v1/memory/check", post(handlers::memory_check_handler))
        .route(
            "/api/v1/codesign/reconcile-labels",
            post(handlers::codesign_reconcile_labels_handler),
        )
        .route(
            "/api/v1/codesign/emit-chaotic-stub",
            post(handlers::codesign_emit_chaotic_stub_handler),
        )
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(cors)
                .layer(DefaultBodyLimit::max(1_048_576))
                .layer(TimeoutLayer::with_status_code(
                    axum::http::StatusCode::REQUEST_TIMEOUT,
                    Duration::from_secs(30),
                )),
        )
}
