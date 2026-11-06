//! Transport-agnostic operations behind the API (mununu MCP, 2026-11): the body of a route,
//! taking the request type and returning the response type, with no `axum` in sight. The HTTP
//! handler (`handlers.rs`) wraps an op in `blocking` + `Json`; the MCP tool (`mcp.rs`) wraps the
//! same op in a blocking task with a budget and a `structuredContent`. One shape, two surfaces —
//! as mununu#536 did for `sv verify-auto --json` and the HTTP body — so neither transport can
//! drift from the other: there is no logic to drift.

use std::time::Instant;

use tracing::info;

use crate::api::error::{ApiError, ApiResult};
use crate::api::handlers::{
    compute_counterstrategy_result, resolve_targets, resolve_template_ref_for_cache,
};
use crate::api::models::*;
use crate::mu_calculus::EvaluationOptions;

/// `POST /api/v1/context/eval` and the MCP `context_eval` tool: evaluate the μ-calculus formulas
/// of a CTXDSL context (all of them, one by name, or a template instance) over their target
/// automata, with the optional counterstrategy and the structural sanity checks.
pub fn context_verify(request: ContextVerifyRequest) -> ApiResult<ContextVerifyResponse> {
    let handler_start = Instant::now();
    let counterstrategy_requested = request.counterstrategy;

    let t0 = Instant::now();

    // Resolve template_ref BEFORE cache lookup so the cache key covers the
    // template instantiation. Repeated requests with the same context +
    // template params hit the cache.
    let (effective_formula, synth_sidecar) = resolve_template_ref_for_cache(
        &request.formula,
        &request.template_ref,
        request.automaton.as_deref().unwrap_or("__default"),
    )?;

    let mut sidecar_strs: Vec<&str> = request
        .sidecars
        .iter()
        .map(|s| s.content.as_str())
        .collect();
    if let Some(ref s) = synth_sidecar {
        sidecar_strs.push(s.as_str());
    }

    let (entry, cache_hit) =
        crate::api::cache::get_or_realize(&request.context.content, &sidecar_strs).map_err(
            |e| ApiError::BadRequest {
                message: format!("Failed to parse/realize context: {}", e),
                details: Some(e.clone()),
            },
        )?;
    let realized = entry.realized.as_ref();
    let realize_ms = t0.elapsed().as_millis();
    info!(
        realize_ms,
        cache_hit,
        counterstrategy = counterstrategy_requested,
        "verify: parse+realize complete"
    );

    // Collect formula–automaton pairs to evaluate
    let mut pairs: Vec<(String, String)> = Vec::new();

    if let Some(ref formula_name) = effective_formula {
        // Specific formula requested
        if !realized.formulas.contains_key(formula_name) {
            return Err(ApiError::BadRequest {
                message: format!("Unknown formula '{}'", formula_name),
                details: None,
            });
        }
        if let Some(ref automaton_name) = request.automaton {
            pairs.push((formula_name.clone(), automaton_name.clone()));
        } else {
            // Use the formula's target automata
            let rf = &realized.formulas[formula_name];
            let automata = resolve_targets(&rf.targets, realized);
            for a in automata {
                pairs.push((formula_name.clone(), a));
            }
        }
    } else {
        // Evaluate ALL explicit user-defined formulas (skip auto-generated structural predicates)
        for (name, rf) in &realized.formulas {
            if rf
                .meta
                .comment
                .as_ref()
                .is_some_and(|c| c.contains("\"type\":\"structural\""))
            {
                continue;
            }
            if let Some(ref automaton_name) = request.automaton {
                pairs.push((name.clone(), automaton_name.clone()));
            } else {
                let automata = resolve_targets(&rf.targets, realized);
                for a in automata {
                    pairs.push((name.clone(), a));
                }
            }
        }
    }

    // Sort for deterministic output
    pairs.sort();

    let eval_options = EvaluationOptions::default();
    let mut results = Vec::new();
    let t2 = Instant::now();

    for (formula_name, automaton_name) in &pairs {
        let rf = &realized.formulas[formula_name];
        let clts = realized
            .context
            .clts(automaton_name)
            .ok_or_else(|| ApiError::BadRequest {
                message: format!("Unknown automaton '{}'", automaton_name),
                details: None,
            })?;
        let env = realized.environment_for(automaton_name);

        let bitvec = realized
            .context
            .evaluate_mu(automaton_name, &rf.formula, &env, Some(&eval_options))
            .map_err(|e| ApiError::Internal {
                message: format!(
                    "μ-calculus evaluation failed for formula '{}' on '{}': {}",
                    formula_name, automaton_name, e
                ),
                source: None,
            })?;

        let mut satisfying_state_names = Vec::new();
        for state_id in clts.states() {
            if bitvec
                .get(state_id.index())
                .map(|bit| *bit)
                .unwrap_or(false)
                && let Some(name) = clts.state_name(state_id)
            {
                satisfying_state_names.push(name.to_string());
            }
        }
        satisfying_state_names.sort();

        let initial_states: Vec<String> = clts
            .initial_states()
            .iter()
            .filter_map(|sid| clts.state_name(*sid).map(|n| n.to_string()))
            .collect();

        let mut initial_satisfying = Vec::new();
        let mut initial_violating = Vec::new();
        for sid in clts.initial_states() {
            if let Some(name) = clts.state_name(*sid) {
                if bitvec.get(sid.index()).map(|bit| *bit).unwrap_or(false) {
                    initial_satisfying.push(name.to_string());
                } else {
                    initial_violating.push(name.to_string());
                }
            }
        }
        initial_satisfying.sort();
        initial_violating.sort();

        let satisfied = initial_violating.is_empty() && !initial_states.is_empty();

        // Compute counterstrategy for failed formulas when requested
        let counterstrategy = if request.counterstrategy && !satisfied {
            compute_counterstrategy_result(
                realized,
                automaton_name,
                &rf.formula,
                &env,
                &eval_options,
                request.minimize_counterstrategy,
            )
        } else {
            None
        };

        results.push(FormulaVerificationResult {
            formula_name: formula_name.clone(),
            automaton: automaton_name.clone(),
            outcome: realizable_outcome(satisfied).to_string(),
            total_states: clts.state_count(),
            satisfying_states: satisfying_state_names.len(),
            initial_states,
            initial_satisfying,
            initial_violating,
            satisfying_state_names,
            counterstrategy,
        });
    }

    let eval_ms = t2.elapsed().as_millis();
    let total_ms = handler_start.elapsed().as_millis();
    info!(
        realize_ms,
        eval_ms,
        total_ms,
        cache_hit,
        formulas = pairs.len(),
        counterstrategy = counterstrategy_requested,
        "verify: complete"
    );

    let all_satisfied = results.iter().all(|r| r.outcome == "holds");

    let mut sanity = Vec::new();
    if request.sanity {
        let mut targets: Vec<String> = match &request.automaton {
            Some(a) => vec![a.clone()],
            None => pairs.iter().map(|(_, a)| a.clone()).collect(),
        };
        targets.sort();
        targets.dedup();
        for a in targets {
            let Some(clts) = realized.context.clts(&a) else {
                return Err(ApiError::BadRequest {
                    message: format!("Unknown automaton '{a}'"),
                    details: None,
                });
            };
            let members: Vec<(String, &crate::clts::Clts<_, _>)> = realized
                .members_of(&a)
                .unwrap_or(&[])
                .iter()
                .filter_map(|m| realized.context.clts(m).map(|c| (m.clone(), c)))
                .collect();
            sanity.push(crate::context::sanity::check(
                &a,
                clts,
                &realized.declared_labels_of(&a),
                &members,
            ));
        }
    }

    let outcome = context_outcome(Some(all_satisfied), sanity.iter().all(|r| r.is_clean()));
    Ok(ContextVerifyResponse {
        success: true,
        outcome: outcome.to_string(),
        results,
        sanity,
    })
}

/// `GET /api/v1/templates` and the MCP `templates` tool: the template catalog, or the templates of
/// one domain.
pub fn templates(domain: Option<&str>) -> ApiResult<serde_json::Value> {
    let registry = crate::adapter::templates::TemplateRegistry::configured(&[]).map_err(|e| {
        ApiError::BadRequest {
            message: format!("the template catalog: {e}"),
            details: None,
        }
    })?;

    if let Some(domain_str) = domain {
        let domain: crate::adapter::templates::TemplateDomain =
            domain_str.parse().map_err(|message| ApiError::BadRequest {
                message,
                details: None,
            })?;
        let mut filtered = registry.for_domain(domain);
        filtered.sort_by(|a, b| a.id.cmp(&b.id));
        return serde_json::to_value(filtered).map_err(catalog_not_serialisable);
    }

    let catalog = registry.catalog();
    serde_json::to_value(catalog).map_err(catalog_not_serialisable)
}

/// A catalogue that does not serialise is an internal error, never an empty `null` body.
fn catalog_not_serialisable(e: serde_json::Error) -> ApiError {
    ApiError::Internal {
        message: format!("the template catalog does not serialise: {e}"),
        source: Some(Box::new(e)),
    }
}
