//! Transport-agnostic operations behind the API (mununu MCP, 2026-11): the body of a route,
//! taking the request type and returning the response type, with no `axum` in sight. The HTTP
//! handler (`handlers.rs`) wraps an op in `blocking` + `Json`; the MCP tool (`mcp.rs`) wraps the
//! same op in a blocking task with a budget and a `structuredContent`. One shape, two surfaces —
//! as mununu#536 did for `sv verify-auto --json` and the HTTP body — so neither transport can
//! drift from the other: there is no logic to drift.

use std::collections::HashSet;
use std::time::Instant;

use tracing::info;

use crate::api::error::{ApiError, ApiResult};
use crate::api::models::*;
use crate::context_dsl::RealizedContext;
use crate::mu_calculus::{Environment, EvaluationOptions, Formula};

/// `POST /api/v1/context/eval` and the MCP `context_eval` tool: evaluate the μ-calculus formulas
/// of a CTXDSL context (all of them, one by name, or a template instance) over their target
/// automata, with the optional counterstrategy and the structural sanity checks.
pub fn context_verify(request: ContextVerifyRequest) -> ApiResult<ContextVerifyResponse> {
    let handler_start = Instant::now();
    let unsupported = request.unsupported_fields();
    if !unsupported.is_empty() {
        return Err(ApiError::BadRequest {
            message: format!(
                "context/eval does not apply {}; remove the field(s) from the request (mununu#862)",
                unsupported.join(", ")
            ),
            details: None,
        });
    }
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
        notes: realized.notes.clone(),
    })
}

/// `GET /api/v1/templates` and the MCP `templates` tool: the template catalog, or the templates of
/// one domain.
pub fn templates(domain: Option<&str>) -> ApiResult<serde_json::Value> {
    let registry =
        crate::adapter::templates::TemplateRegistry::configured_default().map_err(|message| {
            ApiError::BadRequest {
                message,
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

// ---------------------------------------------------------------------------
// mununu#966 — the helpers the handlers and the MCP tools share live here, in the operations
// layer; `handlers` imports them (it used to be the other way round).
// ---------------------------------------------------------------------------

/// Resolve formula targets to a list of automaton names
pub(crate) fn resolve_targets(
    targets: &crate::context_dsl::FormulaTargetsKind,
    realized: &crate::context_dsl::RealizedContext,
) -> Vec<String> {
    use crate::context_dsl::FormulaTargetsKind;
    match targets {
        FormulaTargetsKind::All => realized.context.clts_names(),
        FormulaTargetsKind::Named(names) => names.clone(),
    }
}

/// Compute counterstrategy graph elements for an unsatisfied formula.
///
/// Inverts the formula, evaluates the inverted formula to find the environment's
/// winning region, and builds Cytoscape graph elements for visualization.
pub(crate) fn compute_counterstrategy_result(
    realized: &RealizedContext,
    automaton_name: &str,
    formula: &Formula,
    env: &Environment,
    eval_options: &EvaluationOptions,
    minimize: bool,
) -> Option<CounterstrategyResult> {
    use crate::mu_calculus::invert;

    let clts = realized.context.clts(automaton_name)?;
    let inverted = invert::invert(formula);

    let inverted_result = if minimize {
        realized
            .context
            .evaluate_mu_with_witnesses(automaton_name, &inverted, env, Some(eval_options))
            .ok()
            .map(|(bv, wm)| (bv, Some(wm)))
    } else {
        realized
            .context
            .evaluate_mu(automaton_name, &inverted, env, Some(eval_options))
            .ok()
            .map(|bv| (bv, None))
    };

    inverted_result.map(|(inv_bv, witness_map)| {
        let winning_set: HashSet<usize> = clts
            .states()
            .filter(|sid| inv_bv.get(sid.index()).map(|bit| *bit).unwrap_or(false))
            .map(|sid| sid.index())
            .collect();

        let cs_name = format!("{}_counterstrategy", automaton_name);
        // Graph generation applies strategy extraction and then filters to
        // states reachable from initials via the kept transitions.
        let (graph_elements, reachable) = crate::api::graph::counterstrategy_to_graph_elements(
            clts,
            &cs_name,
            &winning_set,
            minimize,
            witness_map.as_ref(),
        );

        // Report only reachable states in the winning list
        let mut env_winning: Vec<String> = clts
            .states()
            .filter(|sid| reachable.contains(&sid.index()))
            .filter_map(|sid| clts.state_name(sid).map(|n| n.to_string()))
            .collect();
        env_winning.sort();

        CounterstrategyResult {
            environment_winning_states: env_winning,
            graph_elements,
            inverted_formula: format!("{:?}", inverted),
            minimized: minimize,
        }
    })
}

/// Resolve a formula reference (direct name or template_ref) into a name + an
/// optional synthesized sidecar CTXDSL string.
///
/// - When `formula` is given: returns `(Some(name), None)`.
/// - When `template_ref` is given: returns `(Some(formula_name), Some(synth_sidecar_ctxdsl))`,
///   a sidecar whose formula is the CTXDSL `template` CLAUSE for the reference — the one
///   instantiation path, `realize::collect_formulas`, expands it (mununu#966: the API used to
///   instantiate here and write the expanded formula into the sidecar, a second path with its
///   own error texts). The caller passes the sidecar to `cache::get_or_realize`, so the cache
///   key still covers the template and its arguments. The formula is named after the template
///   id (`reachable`), the name the clause would carry in a user's own CTXDSL.
/// - When neither is given: returns `(None, None)` — caller decides whether
///   that's an error.
///
/// Every piece of the request written into CTXDSL text is user input (OWASP: never interpolate
/// it raw). The argument VALUES are string literals through
/// [`crate::context_dsl::string_literal`] (quotes, backslashes and line breaks escaped), so a
/// value cannot close the clause or start another declaration. The template id, the argument
/// KEYS and the automaton name sit in identifier positions, where a literal is not the grammar
/// and escaping is no help: each must be exactly one CTXDSL identifier
/// ([`crate::context_dsl::is_identifier`]) or the request is refused with a `400` before any
/// text is built — an id such as `x { } } } context evil {` used to close the sidecar's blocks
/// and declare its own content.
pub(crate) fn resolve_template_ref_for_cache(
    formula: &Option<String>,
    template_ref: &Option<crate::adapter::templates::TemplateRef>,
    automaton: &str,
) -> Result<(Option<String>, Option<String>), ApiError> {
    if let Some(name) = formula {
        return Ok((Some(name.clone()), None));
    }

    let tref = match template_ref {
        Some(t) => t,
        None => return Ok((None, None)),
    };
    let sidecar =
        template_ref_sidecar(tref, automaton).map_err(|message| ApiError::BadRequest {
            message,
            details: None,
        })?;
    Ok((Some(tref.template.clone()), Some(sidecar)))
}

/// The synthesized sidecar behind a `template_ref`: one formula, named after the template id,
/// over `automaton`, whose body is the `template` clause with the reference's arguments as
/// escaped string literals in key order (deterministic text ⇒ a stable cache key). `Err` names
/// the first of the template id, an argument key or the automaton that is not a CTXDSL
/// identifier; no text is built on it.
pub(crate) fn template_ref_sidecar(
    tref: &crate::adapter::templates::TemplateRef,
    automaton: &str,
) -> Result<String, String> {
    let ident = |what: &str, text: &str| -> Result<(), String> {
        if crate::context_dsl::is_identifier(text) {
            Ok(())
        } else {
            Err(format!(
                "template_ref: the {what} `{text}` is not a CTXDSL identifier (letters, digits \
                 and `_`, not starting with a digit, not a keyword)"
            ))
        }
    };
    ident("template id", &tref.template)?;
    ident("automaton", automaton)?;
    let mut args: Vec<(&String, &String)> = tref.args.iter().collect();
    args.sort();
    for (k, _) in &args {
        ident("argument key", k)?;
    }
    let bindings: String = args
        .iter()
        .map(|(k, v)| format!("        {k} = {};\n", crate::context_dsl::string_literal(v)))
        .collect();
    Ok(format!(
        "context __template_sidecar {{\n  mu_formulas {{\n    formula {id} {{\n      over {automaton};\n      template {id} {{\n{bindings}      }}\n    }}\n  }}\n}}\n",
        id = tref.template
    ))
}

#[cfg(test)]
mod template_ref_sidecar_tests {
    use super::*;
    use crate::adapter::templates::{TemplateRef, TemplateRegistry};
    use crate::context_dsl::ast::FormulaExpr;

    const LIGHT: &str = r#"
context Light {
    alphabet { label tick; }
    automata {
        automaton Light {
            states { state lit initial; state dim; }
            transitions {
                transition lit -> dim on label tick;
                transition dim -> lit on label tick;
            }
        }
    }
}
"#;

    fn tref(template: &str, args: &[(&str, &str)]) -> TemplateRef {
        TemplateRef {
            template: template.to_string(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// mununu#966 — the API's `template_ref` goes through the one instantiation path: the
    /// synthesized sidecar carries the `template` clause, realization expands it, and the
    /// realized formula is byte-for-byte what the registry instantiates for the same reference.
    #[test]
    fn a_template_ref_realizes_to_the_registrys_own_instantiation() {
        let registry = TemplateRegistry::configured_default().expect("the catalog");
        let tref = tref("reachable", &[("TARGET", "dim")]);
        let (name, sidecar) =
            resolve_template_ref_for_cache(&None, &Some(tref.clone()), "Light").expect("resolves");
        let sidecar = sidecar.expect("a synthesized sidecar");
        assert_eq!(name.as_deref(), Some("reachable"));
        let doc = crate::context_dsl::parse(LIGHT).expect("the context parses");
        let side =
            crate::context_dsl::parse(&sidecar).unwrap_or_else(|e| panic!("{sidecar}\n{e:?}"));
        let realized = crate::context_dsl::realize_with_templates(&doc, &[side], &registry)
            .unwrap_or_else(|e| panic!("{sidecar}\n{e:?}"));
        let inst = registry
            .instantiate(&tref)
            .expect("the registry's own instantiation");
        let formula = &realized.formulas["reachable"];
        assert_eq!(formula.raw, inst.formula);
        let expansion = formula
            .template
            .as_ref()
            .expect("recorded as a template expansion");
        assert_eq!(expansion.template, "reachable");
        assert_eq!(
            expansion.args.get("TARGET").map(String::as_str),
            Some("dim")
        );
    }

    /// REPRODUCER (mununu#966 follow-up, OWASP) — the template id, an argument key and the
    /// automaton name sit in IDENTIFIER positions of the synthesized sidecar, where escaping is
    /// no help: on the parent, an id of `x { } } } context evil { … }` reached the text raw,
    /// closed the sidecar's blocks and declared its own context. Each position is refused with a
    /// `400` naming it, and no text is built; a request that is identifiers throughout is not.
    #[test]
    fn a_hostile_template_id_key_or_automaton_is_refused_before_any_text_is_built() {
        let evil =
            "x { } } } context evil { mu_formulas { formula pwn { over Light; body = true; } } } ";
        for (what, template, key, automaton) in [
            ("template id", evil, "TARGET", "Light"),
            ("template id", "a;b", "TARGET", "Light"),
            ("template id", "two\nlines", "TARGET", "Light"),
            ("argument key", "reachable", evil, "Light"),
            ("argument key", "reachable", "K;", "Light"),
            ("argument key", "reachable", "k\n", "Light"),
            ("automaton", "reachable", "TARGET", evil),
            ("automaton", "reachable", "TARGET", "Light;"),
            ("automaton", "reachable", "TARGET", "L\night"),
        ] {
            let tref = tref(template, &[(key, "dim")]);
            let err = resolve_template_ref_for_cache(&None, &Some(tref.clone()), automaton)
                .expect_err("a non-identifier in an identifier position is refused");
            let message = match err {
                ApiError::BadRequest { message, .. } => message,
                other => panic!("{other:?}"),
            };
            assert!(message.contains(what), "{what}: {message}");
            assert!(message.contains("not a CTXDSL identifier"), "{message}");
        }
        // The same request with identifiers throughout resolves to one sidecar with one formula.
        let (_, ok) = resolve_template_ref_for_cache(
            &None,
            &Some(tref("reachable", &[("TARGET", "dim")])),
            "Light",
        )
        .expect("identifiers throughout");
        let ok = ok.expect("a synthesized sidecar");
        let doc = crate::context_dsl::parse(&ok).unwrap_or_else(|e| panic!("{ok}\n{e:?}"));
        assert_eq!(doc.mu_formulas.len(), 1);
    }

    /// The sidecar builder itself refuses by name and builds nothing on a refusal.
    #[test]
    fn the_sidecar_builder_names_the_position_it_refuses() {
        let err = template_ref_sidecar(&tref("a b", &[("TARGET", "dim")]), "Light").unwrap_err();
        assert!(
            err.contains("template id") && err.contains("`a b`"),
            "{err}"
        );
        let err = template_ref_sidecar(&tref("reachable", &[("TARGET", "dim")]), "L;").unwrap_err();
        assert!(err.contains("automaton"), "{err}");
        let err = template_ref_sidecar(&tref("reachable", &[("T }", "dim")]), "Light").unwrap_err();
        assert!(err.contains("argument key"), "{err}");
    }

    /// mununu#966 (OWASP) — argument values are user input written into CTXDSL text: a value
    /// holding `}`, `;`, `"`, a backslash or a line break cannot close the clause or declare
    /// anything; it parses as exactly one binding whose value round-trips.
    #[test]
    fn hostile_argument_values_stay_inside_their_binding() {
        for value in [
            "dim",
            "a}b",
            "x; y",
            "say \"hi\"",
            "back\\slash",
            "two\nlines",
            "} formula evil { over Light; body = true; }",
        ] {
            let sidecar = template_ref_sidecar(&tref("reachable", &[("TARGET", value)]), "Light")
                .expect("a value is escaped, never refused");
            let doc =
                crate::context_dsl::parse(&sidecar).unwrap_or_else(|e| panic!("{sidecar}\n{e:?}"));
            assert_eq!(doc.mu_formulas.len(), 1, "{sidecar}");
            let FormulaExpr::Template(spec) = &doc.mu_formulas[0].body else {
                panic!("{sidecar}: not a template clause");
            };
            assert_eq!(spec.template.name, "reachable");
            assert_eq!(spec.args.len(), 1);
            assert_eq!(spec.args[0].name.name, "TARGET");
            assert_eq!(spec.args[0].value, value, "{sidecar}");
        }
        // Two arguments come out in key order, whatever order the request held them in.
        let a = template_ref_sidecar(&tref("t", &[("B", "2"), ("A", "1")]), "Light")
            .expect("identifiers throughout");
        let b = template_ref_sidecar(&tref("t", &[("A", "1"), ("B", "2")]), "Light")
            .expect("identifiers throughout");
        assert_eq!(a, b);
        assert!(
            a.find("A = \"1\"").unwrap() < a.find("B = \"2\"").unwrap(),
            "{a}"
        );
    }

    /// mununu#1106 item 5 — REPRODUCER: a label controllable in one member and carried by its
    /// partner composes as uncontrollable (mununu#592); the verdict over the composition is read
    /// against that, so the response carries it in `notes`, naming the composition and the
    /// members. On the parent the response has no `notes` key: the demotion reached the log only.
    /// Names no API this change adds (the request is built from JSON, the response read as JSON),
    /// so it compiles on the parent and fails there.
    #[test]
    fn x1106_a_controllability_demotion_reaches_the_verify_response_as_a_note() {
        let src = r#"
context C {
    automata {
        automaton Ctl {
            controllable { label go; label wait; }
            states { state C0 initial; state C1; }
            transitions { transition C0 -> C1 on label go; transition C0 -> C0 on label wait; transition C1 -> C1 on label wait; }
        }
        automaton Plant {
            controllable { }
            states { state P0 initial; state Bad; }
            transitions { transition P0 -> Bad on label go; transition P0 -> P0 on label tick; transition Bad -> Bad on label tick; }
        }
    }
    composition { asynchronous Sys { members [Ctl, Plant]; } }
    mu_formulas {
        formula live { over Sys; body = nu X. (true && [] X); }
    }
}
"#;
        let request: ContextVerifyRequest = serde_json::from_value(serde_json::json!({
            "context": { "name": "demotion.ctx", "content": src },
            "formula": "live",
            "automaton": "Sys",
        }))
        .expect("a verify request");
        let response =
            serde_json::to_value(context_verify(request).expect("verifies")).expect("serializes");
        assert_eq!(response["success"], true, "{response}");
        let notes = response["notes"]
            .as_array()
            .unwrap_or_else(|| panic!("the response carries `notes`: {response}"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        let note = notes[0].as_str().expect("a string");
        assert!(
            note.starts_with("composition `Sys`: [mununu#592] label `go` is controllable in `Ctl` but `Plant` carries it too"),
            "{note}"
        );
        // The MCP text block beside the structured result says the same.
        let summary = crate::api::mcp::summary(
            &context_verify(
                serde_json::from_value(serde_json::json!({
                    "context": { "name": "demotion.ctx", "content": src },
                    "formula": "live",
                    "automaton": "Sys",
                }))
                .expect("a verify request"),
            )
            .expect("verifies"),
        );
        assert!(
            summary.contains("\nnote: composition `Sys`: [mununu#592]"),
            "{summary}"
        );
    }
}
