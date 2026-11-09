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
    ui_dir: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = create_router_with_ui(ui_dir);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("Server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// The API router plus, when `ui_dir` names a directory, its static files at `/` — the
/// mununu-ui build served from the same origin as the API (roadmap H5, the on-prem bundle: one
/// port, no CORS story). Paths that are not files fall back to `index.html` so the client-side
/// router works on a reload; `/api/v1/*` is matched first and stays the API's.
pub fn create_router_with_ui(ui_dir: Option<&std::path::Path>) -> Router {
    let app = create_router();
    match ui_dir {
        None => app,
        Some(dir) => {
            use tower_http::services::{ServeDir, ServeFile};
            // `fallback`, not `not_found_service`: the latter wraps the fallback in a 404 (a
            // "not found page"); a client-side route must come back 200 with the index.
            let index = ServeFile::new(dir.join("index.html"));
            // mununu#940 — an unknown `/api/…` path is an API error, not a client-side route:
            // without this, a mistyped route came back as the UI's index.html with 200.
            app.route("/api", axum::routing::any(api_not_found))
                .route("/api/{*rest}", axum::routing::any(api_not_found))
                .fallback_service(ServeDir::new(dir).fallback(index))
        }
    }
}

/// mununu#940 — the JSON 404 for an `/api/…` path no route matched (only reached under `--ui-dir`;
/// without a UI the router's own 404 answers).
async fn api_not_found(uri: axum::http::Uri) -> crate::api::error::ApiError {
    crate::api::error::ApiError::NotFound {
        resource: format!("API route `{}`", uri.path()),
    }
}

/// mununu#934 — `http(s)://localhost`, `127.0.0.1` or `[::1]`, any port: a page served from this
/// machine. Anything else (including `localhost.example.com`) is not loopback.
pub(crate) fn is_loopback_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let host = if let Some(v6) = rest.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, tail)) if tail.is_empty() || tail.starts_with(':') => {
                return h == "::1" && port_ok(tail);
            }
            _ => return false,
        }
    } else {
        match rest.split_once(':') {
            Some((h, port)) => {
                if !port_ok(&format!(":{port}")) {
                    return false;
                }
                h
            }
            None => rest,
        }
    };
    host == "localhost" || host == "127.0.0.1"
}

/// An origin's port part: empty or `:` + digits.
fn port_ok(tail: &str) -> bool {
    tail.is_empty()
        || tail
            .strip_prefix(':')
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Create the API router. Public so the MCP seam test can drive a route in-process
/// (`tower::ServiceExt::oneshot`) and compare its body with the tool's `structuredContent`.
pub fn create_router() -> Router {
    // mununu#934 — the API has no authentication and runs tools, so a browser page from any
    // origin must not be able to read its answers: loopback origins (the UI dev server, a local
    // client) and the deployment's `[server] cors_origins` only. The bundle's UI is same-origin
    // and needs neither. A configuration error leaves the loopback default (the server's start
    // already reports it through the licence status's `reason`).
    let allowed = crate::deployment::current()
        .map(|c| c.cors_origins)
        .unwrap_or_default();
    let cors = CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::predicate(
            move |origin: &axum::http::HeaderValue, _| {
                origin
                    .to_str()
                    .is_ok_and(|o| is_loopback_origin(o) || allowed.iter().any(|a| a == o))
            },
        ))
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/v1/health", get(handlers::health_check))
        .route("/api/v1/licence", get(handlers::licence_handler))
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
        // sound engine (exact ⊕ native ⊕ interpolation ⊕ btormc ⊕ Pono ⊕ rIC3), merged under the
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
        // Roadmap J2 — the same body, the plan only: per-property cost estimates, no engine.
        .route("/api/v1/sv/plan", post(handlers::sv_plan_handler))
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
        // SV-direct partial-write preflight (mununu#469) — lift + report the
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

#[cfg(test)]
mod ui_dir_tests {
    //! H5 at the seam: the UI directory is served at `/`, assets by path, unknown paths fall
    //! back to `index.html`, and the API keeps `/api/v1/*`; without a directory `/` is a 404.
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get(app: axum::Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn the_ui_directory_is_served_beside_the_api_with_an_index_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><title>mununu ui</title>",
        )
        .expect("index");
        std::fs::create_dir(dir.path().join("assets")).expect("assets dir");
        std::fs::write(
            dir.path().join("assets").join("app.js"),
            "console.log('ui')",
        )
        .expect("asset");
        let app = super::create_router_with_ui(Some(dir.path()));
        let (status, body) = get(app.clone(), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("mununu ui"), "{body}");
        let (status, body) = get(app.clone(), "/assets/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("console.log"), "{body}");
        // A client-side route reloads to the index, not to a 404.
        let (status, body) = get(app.clone(), "/runs/42").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("mununu ui"), "{body}");
        // The API is matched first and stays the API's.
        let (status, body) = get(app, "/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""), "{body}");
    }

    #[tokio::test]
    async fn without_a_ui_directory_the_root_is_not_found() {
        let (status, _) = get(super::create_router_with_ui(None), "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn get_with_origin(
        app: axum::Router,
        uri: &str,
        origin: &str,
    ) -> Option<axum::http::HeaderValue> {
        let response = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("origin", origin)
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("the router answers");
        response
            .headers()
            .get("access-control-allow-origin")
            .cloned()
    }

    /// mununu#940 — under `--ui-dir`, an unknown `/api/…` path is a JSON 404, not the UI page.
    #[tokio::test]
    async fn an_unknown_api_path_is_a_json_404_under_the_ui() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("index.html"), "<title>mununu ui</title>").expect("index");
        let app = super::create_router_with_ui(Some(dir.path()));
        for uri in ["/api/v1/no-such-route", "/api", "/api/v2/health"] {
            let (status, body) = get(app.clone(), uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
            assert!(
                body.contains("NOT_FOUND") && !body.contains("mununu ui"),
                "{uri}: {body}"
            );
        }
        // The real routes still answer.
        let (status, _) = get(app, "/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
    }

    /// mununu#936 — `..` and its percent-encoded form cannot leave the UI directory. (A symlink
    /// inside the directory is followed: the directory is the operator's own UI build, so a link
    /// in it is the operator's choice; the bundle ships none.)
    #[tokio::test]
    async fn traversal_out_of_the_ui_directory_is_refused() {
        let outer = tempfile::tempdir().expect("tempdir");
        std::fs::write(outer.path().join("secret.txt"), "TOP SECRET").expect("secret");
        let ui = outer.path().join("ui");
        std::fs::create_dir(&ui).expect("ui dir");
        std::fs::write(ui.join("index.html"), "<title>mununu ui</title>").expect("index");
        let app = super::create_router_with_ui(Some(&ui));
        for uri in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/assets/%2e%2e/%2e%2e/secret.txt",
            "/..%2fsecret.txt",
        ] {
            let (_, body) = get(app.clone(), uri).await;
            assert!(
                !body.contains("TOP SECRET"),
                "{uri} served a file outside the UI directory"
            );
        }
    }

    /// mununu#934 — a loopback page may read the API's answers; a page from anywhere else may not.
    #[tokio::test]
    async fn only_loopback_origins_get_cors_access_by_default() {
        let app = super::create_router();
        for ok in [
            "http://localhost:5173",
            "http://127.0.0.1:8080",
            "http://[::1]:3000",
        ] {
            assert_eq!(
                get_with_origin(app.clone(), "/api/v1/health", ok)
                    .await
                    .as_ref()
                    .and_then(|v| v.to_str().ok()),
                Some(ok),
                "{ok}"
            );
        }
        for bad in [
            "https://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "null",
        ] {
            assert!(
                get_with_origin(app.clone(), "/api/v1/health", bad)
                    .await
                    .is_none(),
                "{bad} must not be allowed"
            );
        }
    }

    #[test]
    fn loopback_origins_are_exactly_localhost_127_0_0_1_and_ipv6_loopback() {
        use super::is_loopback_origin as l;
        assert!(l("http://localhost") && l("https://localhost:8443") && l("http://127.0.0.1:1"));
        assert!(l("http://[::1]") && l("http://[::1]:5173"));
        assert!(!l("http://localhost:") && !l("http://localhost:80x") && !l("ftp://localhost"));
        assert!(!l("http://[::2]:80") && !l("http://[::1]x") && !l("http://localhost.example"));
    }
}
