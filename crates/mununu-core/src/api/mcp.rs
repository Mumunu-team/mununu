//! The MCP server (`mununu mcp`, 2026-11): a third transport of the HTTP API, not a new
//! capability. The one tool that decides anything, `context_eval`, takes the body of
//! `POST /api/v1/context/eval` and returns that route's response as `structuredContent`, byte for
//! byte — both go through [`crate::api::ops::context_verify`], so there is no logic here to drift
//! (mununu#536's rule: one shape, two surfaces). `templates` mirrors `GET /api/v1/templates`.
//!
//! `engine:` unchanged — the tool calls the same CTXDSL evaluation the HTTP handler calls (the
//! explicit μ-calculus over the realized CLTS; the counterstrategy by formula inversion).
//!
//! The tools are declared by hand from the schemas the API already publishes
//! (`docs/api-schemas/context-eval-{request,response}.schema.json`, schemars 0.8), not by rmcp's
//! macros (schemars 1.x): the advertised `inputSchema` / `outputSchema` are the committed files, and
//! [`tests::the_tools_advertise_the_api_schemas`] keeps them identical.
//!
//! Failure paths (policy §Failure Paths — silence is a defect): a design problem is a tool result
//! with `isError: true` whose text and `structuredContent` are the HTTP 400 body
//! (`{success: false, error: {code, message, details}}`), never a protocol-level error; a call over
//! the budget is `isError: true` naming the budget; a tool name the server does not have, or
//! arguments that do not deserialize, are protocol-level `invalid_params` — those are client
//! defects, not design findings.

use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData as McpError;
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    Tool,
};
use rmcp::service::RequestContext;
use serde_json::Value;

use crate::api::error::ApiError;
use crate::api::models::{ContextVerifyRequest, ContextVerifyResponse};
use crate::api::ops;
use crate::api::schema::{context_eval_request_schema, context_eval_response_schema};

/// The tool that mirrors `POST /api/v1/context/eval`.
pub const CONTEXT_EVAL: &str = "context_eval";
/// The tool that mirrors `GET /api/v1/templates`.
pub const TEMPLATES: &str = "templates";
/// The default per-call budget: the HTTP layer's `TimeoutLayer` value (`api/server.rs`).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The server: two tools over one op each, a wall-clock budget per call.
#[derive(Clone, Debug)]
pub struct MununuMcp {
    request_timeout: Duration,
}

impl MununuMcp {
    pub fn new(request_timeout: Duration) -> Self {
        Self { request_timeout }
    }

    /// The tools the server advertises, with their schemas taken from the API's own
    /// (`inputSchema` = the request body's, `outputSchema` = the response's).
    pub fn tools() -> Vec<Tool> {
        let context_eval = Tool::new(
            CONTEXT_EVAL,
            "Evaluate the mu-calculus formulas of a CTXDSL context — all of them, one by name, or \
             a property-template instance — over their target automata; optionally the \
             environment's counterstrategy for a violated formula and the structural sanity \
             checks. The input is the body of `POST /api/v1/context/eval`, the structured result \
             is that route's response, byte for byte. Sources come inline in `context.content` \
             (and `sidecars[].content`); the server opens no file.",
            schema_object(context_eval_request_schema()),
        )
        .with_raw_output_schema(Arc::new(schema_object(context_eval_response_schema())));
        let templates = Tool::new(
            TEMPLATES,
            "The property-template catalogue (`GET /api/v1/templates`): every template, or the \
             templates of one `domain` (rtl, agentic, software, synthesis, universal). Read-only; \
             a `template_ref` of `context_eval` names one of these.",
            templates_input_schema(),
        );
        vec![context_eval, templates]
    }
}

/// A schemars document as the JSON object rmcp carries; a non-object schema (never the case for
/// a struct) is wrapped so nothing is dropped.
fn schema_object(schema: Value) -> JsonObject {
    match schema {
        Value::Object(map) => map,
        other => {
            let mut map = JsonObject::new();
            map.insert("schema".to_string(), other);
            map
        }
    }
}

fn templates_input_schema() -> JsonObject {
    schema_object(serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "TemplatesQuery",
        "type": "object",
        "properties": {
            "domain": {
                "description": "Filter by domain: rtl, agentic, software, synthesis, universal. Omit for the whole catalogue.",
                "type": ["string", "null"]
            }
        },
        "additionalProperties": false
    }))
}

/// The compact text block beside the structured result: the outcome, then one line per result.
pub fn summary(response: &ContextVerifyResponse) -> String {
    let mut text = response.outcome.clone();
    for r in &response.results {
        text.push('\n');
        text.push_str(&format!(
            "{} @ {}: {}",
            r.formula_name, r.automaton, r.outcome
        ));
    }
    text
}

/// A design problem (or an internal error) as the tool result the HTTP surface's body implies:
/// `isError: true`, the text and the structured content both the `{success, error}` body.
fn tool_error(error: &ApiError) -> CallToolResult {
    let (_, body) = error.response_body();
    let value = serde_json::to_value(&body).unwrap_or_else(|e| {
        serde_json::json!({
            "success": false,
            "error": { "code": "INTERNAL_ERROR", "message": format!("the error body did not serialize: {e}") }
        })
    });
    let text = value.to_string();
    let mut result = CallToolResult::structured_error(value);
    result.content = vec![ContentBlock::text(text)];
    result
}

/// A successful op as the tool result: the response as `structuredContent`, the summary as text.
fn tool_success<T: serde::Serialize>(value: &T, text: String) -> CallToolResult {
    let mut result = match serde_json::to_value(value) {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => {
            return tool_error(&ApiError::Internal {
                message: format!("the response did not serialize: {e}"),
                source: None,
            });
        }
    };
    result.content = vec![ContentBlock::text(text)];
    result
}

/// Run an op on the blocking pool under the call's budget. The budget changes what the tool
/// REPORTS, never what the engine decides: a call over it is an error naming the budget, and the
/// engine thread finishes on its own, as a timed-out HTTP request's does (`spawn_blocking` cannot
/// be cancelled).
async fn run_op<T, F>(budget: Duration, op: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    let task = tokio::task::spawn_blocking(op);
    match tokio::time::timeout(budget, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(join)) => Err(ApiError::Internal {
            message: format!("the tool's task panicked or was cancelled: {join}"),
            source: None,
        }),
        Err(_) => Err(ApiError::Internal {
            message: format!(
                "the request exceeded its budget of {} ms (`mununu mcp --request-timeout`); the \
                 engine finishes in the background and the verdict is not reported",
                budget.as_millis()
            ),
            source: None,
        }),
    }
}

impl ServerHandler for MununuMcp {
    fn get_info(&self) -> InitializeResult {
        let mut info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "mununu verifies CTXDSL models of reactive systems. `context_eval` takes the \
                 CTXDSL sources inline and returns the verdict of every formula (holds / \
                 violated), exactly the JSON of `POST /api/v1/context/eval`; `templates` lists \
                 the property templates `template_ref` can instantiate.",
            );
        info.server_info = Implementation::new("mununu", env!("CARGO_PKG_VERSION"));
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(Self::tools())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        let budget = self.request_timeout;
        async move {
            let arguments = request.arguments.unwrap_or_default();
            let result = match request.name.as_ref() {
                CONTEXT_EVAL => {
                    let req: ContextVerifyRequest = serde_json::from_value(Value::Object(
                        arguments,
                    ))
                    .map_err(|e| {
                        McpError::invalid_params(
                            format!(
                                "{CONTEXT_EVAL}: the arguments are not a context/eval request: {e}"
                            ),
                            None,
                        )
                    })?;
                    match run_op(budget, move || {
                        // The budget test's seam: a request whose context carries the test's
                        // marker name sleeps past any budget, so "over the budget" is decided
                        // by the test, never by how fast this box evaluates a small model.
                        #[cfg(test)]
                        tests::slow_op_hook(&req);
                        ops::context_verify(req)
                    })
                    .await
                    {
                        Ok(response) => tool_success(&response, summary(&response)),
                        Err(error) => tool_error(&error),
                    }
                }
                TEMPLATES => {
                    let domain = match arguments.get("domain") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(s)) => Some(s.clone()),
                        Some(other) => {
                            return Err(McpError::invalid_params(
                                format!("{TEMPLATES}: `domain` must be a string, got {other}"),
                                None,
                            ));
                        }
                    };
                    match run_op(budget, move || ops::templates(domain.as_deref())).await {
                        Ok(catalog) => {
                            let text = format!(
                                "{TEMPLATES}: {} bytes of catalogue",
                                catalog.to_string().len()
                            );
                            tool_success(&catalog, text)
                        }
                        Err(error) => tool_error(&error),
                    }
                }
                other => {
                    return Err(McpError::invalid_params(
                        format!(
                            "unknown tool '{other}' (the tools are {CONTEXT_EVAL} and {TEMPLATES})"
                        ),
                        None,
                    ));
                }
            };
            Ok(result.into())
        }
    }
}

/// Serve on stdin/stdout until the client closes the session. Logs go to stderr (the CLI routes
/// them there under `mununu mcp`); stdout is the protocol channel.
pub async fn serve_stdio(
    request_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = MununuMcp::new(request_timeout)
        .serve(rmcp::transport::io::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    /// A context named this way makes the op sleep well past the budget test's 1 ms budget —
    /// request-scoped, so the other tests' calls (and the seam test's timing) are untouched.
    pub(super) const SLOW_CONTEXT_NAME: &str = "__mununu_test_slow__.ctxdsl";

    pub(super) fn slow_op_hook(req: &ContextVerifyRequest) {
        if req.context.name == SLOW_CONTEXT_NAME {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    const COUNTERS: &str = include_str!("../../../../examples/counters/counters.ctxdsl");
    const COUNTERS_PROPERTIES: &str =
        include_str!("../../../../examples/counters/counters_properties.ctxdsl");
    /// The arbiter example: one automaton, six formulas — enough evaluation (with the
    /// counterstrategy and the sanity checks) that a 1 ms budget is exceeded every time.
    const ARBITER: &str = include_str!("../../../../examples/hw/arbiter.ctxdsl");
    /// An arbiter with a formula that is violated on purpose (`body = false`), so the
    /// counterstrategy path has something to compute, and a second automaton for `sanity`.
    const VIOLATED: &str = r#"
context violated_props {
    mu_formulas {
        formula never_true {
            over Counter;
            body = false;
        }
    }
}
"#;

    /// The tools advertise exactly the schema files the API publishes — the drift test pins
    /// those files to the types, and this pins the tool to the same documents.
    #[test]
    fn the_tools_advertise_the_api_schemas() {
        let tools = MununuMcp::tools();
        let eval = tools
            .iter()
            .find(|t| t.name == CONTEXT_EVAL)
            .expect("context_eval is advertised");
        assert_eq!(
            Value::Object((*eval.input_schema).clone()),
            context_eval_request_schema()
        );
        assert_eq!(
            eval.output_schema
                .as_ref()
                .map(|s| Value::Object((**s).clone())),
            Some(context_eval_response_schema())
        );
        let templates = tools
            .iter()
            .find(|t| t.name == TEMPLATES)
            .expect("templates is advertised");
        assert!(templates.output_schema.is_none());
        assert_eq!(tools.len(), 2, "exactly the two tools: {tools:?}");
    }

    #[test]
    fn the_summary_is_the_outcome_then_one_line_per_result() {
        let response = ContextVerifyResponse {
            success: true,
            outcome: "violated".into(),
            results: vec![
                crate::api::models::FormulaVerificationResult {
                    formula_name: "a".into(),
                    automaton: "X".into(),
                    outcome: "holds".into(),
                    total_states: 1,
                    satisfying_states: 1,
                    initial_states: vec![],
                    initial_satisfying: vec![],
                    initial_violating: vec![],
                    satisfying_state_names: vec![],
                    counterstrategy: None,
                },
                crate::api::models::FormulaVerificationResult {
                    formula_name: "b".into(),
                    automaton: "Y".into(),
                    outcome: "violated".into(),
                    total_states: 1,
                    satisfying_states: 0,
                    initial_states: vec![],
                    initial_satisfying: vec![],
                    initial_violating: vec![],
                    satisfying_state_names: vec![],
                    counterstrategy: None,
                },
            ],
            sanity: vec![],
        };
        assert_eq!(
            summary(&response),
            "violated\na @ X: holds\nb @ Y: violated"
        );
    }

    /// The HTTP body of `POST /api/v1/context/eval` for `request`, through the real router.
    async fn http_body(request: &Value) -> (StatusCode, Value) {
        let app = crate::api::server::create_router();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/context/eval")
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .expect("a request"),
            )
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        (
            status,
            serde_json::from_slice(&bytes).expect("the body is JSON"),
        )
    }

    /// An in-process client over a tokio duplex pipe, the server on the other end.
    async fn client_and_server(
        budget: Duration,
    ) -> (
        rmcp::service::RunningService<rmcp::RoleClient, ()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server = tokio::spawn(async move {
            let running = MununuMcp::new(budget)
                .serve((server_read, server_write))
                .await
                .expect("the server initializes");
            let _ = running.waiting().await;
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        let client = ().serve((client_read, client_write)).await.expect("the client initializes");
        (client, server)
    }

    async fn call(
        client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
        request: &Value,
    ) -> CallToolResult {
        let arguments = match request {
            Value::Object(map) => map.clone(),
            _ => panic!("a request is an object"),
        };
        client
            .call_tool(CallToolRequestParams::new(CONTEXT_EVAL).with_arguments(arguments))
            .await
            .expect("the tool answers")
    }

    fn request(context: &str, sidecars: &[&str], extra: Value) -> Value {
        let mut req = serde_json::json!({
            "context": { "name": "counters.ctxdsl", "content": context },
            "sidecars": sidecars.iter().enumerate().map(|(i, s)| serde_json::json!({ "name": format!("sidecar{i}.ctxdsl"), "content": s })).collect::<Vec<_>>(),
        });
        if let (Value::Object(r), Value::Object(e)) = (&mut req, extra) {
            r.extend(e);
        }
        req
    }

    /// The seam (policy §Test the Layer the Bug Lives In): the tool's `structuredContent` IS the
    /// HTTP body for the same request — every case the route distinguishes, plus the one the
    /// transport adds (the budget).
    #[tokio::test]
    async fn the_tool_returns_the_http_body_for_the_same_request() {
        let (client, server) = client_and_server(DEFAULT_REQUEST_TIMEOUT).await;
        let tools = client.list_all_tools().await.expect("tools are listed");
        assert_eq!(
            tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>(),
            vec![CONTEXT_EVAL.to_string(), TEMPLATES.to_string()]
        );

        let cases: Vec<(&str, Value)> = vec![
            (
                "all formulas",
                request(COUNTERS, &[COUNTERS_PROPERTIES], serde_json::json!({})),
            ),
            (
                "one formula",
                request(
                    COUNTERS,
                    &[COUNTERS_PROPERTIES],
                    serde_json::json!({ "formula": "reachability" }),
                ),
            ),
            (
                "template_ref",
                request(
                    COUNTERS,
                    &[],
                    serde_json::json!({ "template_ref": { "template": "no_deadlock" }, "automaton": "Counter" }),
                ),
            ),
            (
                "counterstrategy",
                request(
                    COUNTERS,
                    &[VIOLATED],
                    serde_json::json!({ "counterstrategy": true, "minimize_counterstrategy": true }),
                ),
            ),
            (
                "sanity",
                request(
                    COUNTERS,
                    &[COUNTERS_PROPERTIES],
                    serde_json::json!({ "sanity": true }),
                ),
            ),
        ];
        let mut mcp_ns = 0u128;
        let mut http_ns = 0u128;
        for (label, req) in &cases {
            let t0 = std::time::Instant::now();
            let result = call(&client, req).await;
            mcp_ns += t0.elapsed().as_nanos();
            let t1 = std::time::Instant::now();
            let (status, body) = http_body(req).await;
            http_ns += t1.elapsed().as_nanos();
            assert_eq!(status, StatusCode::OK, "{label}: {body}");
            assert_ne!(result.is_error, Some(true), "{label}: {result:?}");
            assert_eq!(
                result.structured_content.as_ref(),
                Some(&body),
                "{label}: the structured content is the HTTP body"
            );
            let text = match result.content.first() {
                Some(ContentBlock::Text(t)) => t.text.clone(),
                other => panic!("{label}: a text block, got {other:?}"),
            };
            let outcome = body["outcome"].as_str().expect("an outcome");
            assert!(text.starts_with(outcome), "{label}: {text}");
            assert_eq!(
                text.lines().count(),
                1 + body["results"].as_array().map(|r| r.len()).unwrap_or(0),
                "{label}: one line per result: {text}"
            );
        }
        eprintln!(
            "MCP vs HTTP, {} calls in-process: mcp {:.2} ms, http {:.2} ms per call",
            cases.len(),
            mcp_ns as f64 / 1e6 / cases.len() as f64,
            http_ns as f64 / 1e6 / cases.len() as f64
        );

        // A CTXDSL syntax error: the same `{success, error}` body on both surfaces, as a tool
        // error, not a protocol error.
        let broken = request("context broken {", &[], serde_json::json!({}));
        let result = call(&client, &broken).await;
        let (status, body) = http_body(&broken).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert_eq!(result.structured_content.as_ref(), Some(&body));
        match result.content.first() {
            Some(ContentBlock::Text(t)) => {
                assert_eq!(
                    serde_json::from_str::<Value>(&t.text).expect("the text is the JSON body"),
                    body
                );
            }
            other => panic!("a text block, got {other:?}"),
        }
        assert_eq!(body["success"], Value::Bool(false));
        assert_eq!(body["error"]["code"], Value::String("BAD_REQUEST".into()));

        // `templates` mirrors the catalogue route's op.
        let catalog = client
            .call_tool(CallToolRequestParams::new(TEMPLATES))
            .await
            .expect("the catalogue answers");
        assert_ne!(catalog.is_error, Some(true));
        assert_eq!(
            catalog.structured_content,
            Some(ops::templates(None).expect("the catalogue loads"))
        );
        let rtl = client
            .call_tool(
                CallToolRequestParams::new(TEMPLATES)
                    .with_arguments(schema_object(serde_json::json!({ "domain": "rtl" }))),
            )
            .await
            .expect("the domain answers");
        assert_eq!(
            rtl.structured_content,
            Some(ops::templates(Some("rtl")).expect("the domain loads"))
        );

        client.cancel().await.expect("the client closes");
        let _ = server.await;
    }

    /// The budget is the transport's: over it, the tool reports an error naming the budget.
    #[tokio::test]
    async fn a_call_over_the_budget_is_a_tool_error_naming_it() {
        let (client, server) = client_and_server(Duration::from_millis(1)).await;
        // The first version of this test counted on a real evaluation taking more than a
        // millisecond; the optimized test profile answers the arbiter's six formulas, the
        // counterstrategy and the sanity checks in less, so the budget never fired. The op now
        // sleeps on the marker context name (`slow_op_hook`), and the budget fires every time.
        let mut req = request(ARBITER, &[], serde_json::json!({}));
        req["context"]["name"] = serde_json::json!(SLOW_CONTEXT_NAME);
        let result = call(&client, &req).await;
        assert_eq!(result.is_error, Some(true), "{result:?}");
        let text = match result.content.first() {
            Some(ContentBlock::Text(t)) => t.text.clone(),
            other => panic!("a text block, got {other:?}"),
        };
        assert!(
            text.contains("budget of 1 ms"),
            "the error names the budget: {text}"
        );
        client.cancel().await.expect("the client closes");
        let _ = server.await;
    }

    /// Unknown tools and malformed arguments are protocol errors (client defects), not findings.
    #[tokio::test]
    async fn an_unknown_tool_or_a_malformed_request_is_a_protocol_error() {
        let (client, server) = client_and_server(DEFAULT_REQUEST_TIMEOUT).await;
        let unknown = client
            .call_tool(CallToolRequestParams::new("sv_verify_auto"))
            .await;
        assert!(unknown.is_err(), "{unknown:?}");
        let malformed = client
            .call_tool(
                CallToolRequestParams::new(CONTEXT_EVAL).with_arguments(schema_object(
                    serde_json::json!({ "context": "not an object" }),
                )),
            )
            .await;
        assert!(malformed.is_err(), "{malformed:?}");
        client.cancel().await.expect("the client closes");
        let _ = server.await;
    }
}
