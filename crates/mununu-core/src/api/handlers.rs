//! HTTP handlers for REST API endpoints.
//!
//! This module provides async handlers that wrap existing CLI functions
//! and convert file-based operations to in-memory content processing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use axum::Json;
use tracing::info;

use crate::api::error::{ApiError, ApiResult};
use crate::api::graph::generate_graphs;
use crate::api::models::*;
use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelId};
use crate::context::{ControllerSynthesisOptions, DiagnosticsOptions as ContextDiagnosticsOptions};
use crate::context_dsl::RealizedContext;
use crate::guard::sanitize_identifier;
use crate::mu_calculus::{Environment, EvaluationOptions, Formula};

/// P22 — run a synchronous, CPU-heavy handler body on the blocking threadpool so
/// it does not stall a tokio worker. Verification (parse/realize/BMC/CEGAR/
/// synthesis) is CPU-bound and can run for seconds; on the async executor that
/// blocks every other request sharing the worker. The heavy body lives in a
/// sibling `*_impl` fn; this wrapper just moves it off-thread and flattens the
/// `JoinError` (task panic/cancel) into an `ApiError`.
async fn blocking<T, F>(f: F) -> ApiResult<T>
where
    F: FnOnce() -> ApiResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal {
            message: format!("handler task panicked or was cancelled: {e}"),
            source: None,
        })?
}

/// Health check endpoint
pub async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "mununu-api"
    }))
}

/// List available property templates.
pub async fn templates_handler(
    query: axum::extract::Query<TemplatesQuery>,
) -> Json<serde_json::Value> {
    let registry = crate::adapter::templates::TemplateRegistry::builtin();

    if let Some(domain_str) = &query.domain {
        use crate::adapter::templates::TemplateDomain;
        let domain = match domain_str.as_str() {
            "rtl" => Some(TemplateDomain::Rtl),
            "agentic" => Some(TemplateDomain::Agentic),
            "software" => Some(TemplateDomain::Software),
            "synthesis" => Some(TemplateDomain::Synthesis),
            "universal" => Some(TemplateDomain::Universal),
            _ => None,
        };
        if let Some(d) = domain {
            let filtered: Vec<_> = registry.for_domain(d);
            return Json(serde_json::to_value(filtered).unwrap_or_default());
        }
    }

    let catalog = registry.catalog();
    Json(serde_json::to_value(catalog).unwrap_or_default())
}

/// Query parameters for the templates endpoint.
#[derive(Debug, serde::Deserialize)]
pub struct TemplatesQuery {
    pub domain: Option<String>,
}

/// Summarize context (automata, formulas, controllers)
pub async fn context_summarize_handler(
    Json(request): Json<ContextSummarizeRequest>,
) -> ApiResult<Json<ContextSummarizeResponse>> {
    blocking(move || context_summarize_handler_impl(request)).await
}

fn context_summarize_handler_impl(
    request: ContextSummarizeRequest,
) -> ApiResult<Json<ContextSummarizeResponse>> {
    let handler_start = Instant::now();

    // Cache parse + realize. Subsequent calls with identical content reuse the
    // realized result, sidestepping CLTS construction / abstraction unrolling.
    let t0 = Instant::now();
    let sidecar_strs: Vec<&str> = request
        .sidecars
        .iter()
        .map(|s| s.content.as_str())
        .collect();
    let (entry, cache_hit) =
        crate::api::cache::get_or_realize(&request.context.content, &sidecar_strs).map_err(
            |e| ApiError::BadRequest {
                message: format!("Failed to parse/realize context: {}", e),
                details: Some(e.clone()),
            },
        )?;
    let realize_ms = t0.elapsed().as_millis();
    let context_doc = entry.context_doc.as_ref();
    let sidecar_docs = entry.sidecar_docs.as_ref();
    let realized = entry.realized.as_ref();
    info!(realize_ms, cache_hit, "summarize: parse+realize complete");

    // Build summary — include both direct automata and compositions
    let mut automata_names: Vec<String> = context_doc
        .automata
        .iter()
        .map(|a| a.name.name.clone())
        .collect();
    for doc in std::iter::once(context_doc).chain(sidecar_docs.iter()) {
        for comp in &doc.compositions {
            automata_names.push(comp.name.name.clone());
        }
    }

    let automata: Vec<AutomatonSummary> = automata_names
        .iter()
        .filter_map(|name| {
            realized.context.clts(name).map(|clts| AutomatonSummary {
                name: name.clone(),
                states_count: clts.states().count(),
                transitions_count: clts.states().map(|sid| clts.outgoing(sid).len()).sum(),
            })
        })
        .collect();

    // Report declared controllers (declarations only — no synthesis execution).
    // Per CLAUDE.md governance: "Never run controller synthesis in summary/informational
    // endpoints." Synthesis is expensive (state-space exploration) and belongs only in
    // the synthesis endpoint.
    let mut controllers = Vec::new();
    for rc in realized.controllers.values() {
        controllers.push(ControllerSummary {
            name: rc.name.clone(),
            source: rc.source.clone(),
            formula: rc.formula.clone(),
            realizable: false,
            states_count: 0,
            transitions_count: 0,
        });
    }

    let controllers_count = controllers.len();
    let summary = ContextSummary {
        context_name: context_doc.name.name.clone(),
        automata,
        formulas_count: realized.formulas.len(),
        controllers_count: realized.controllers.len(),
        controllers,
    };
    let total_ms = handler_start.elapsed().as_millis();
    info!(
        realize_ms,
        total_ms,
        cache_hit,
        controllers = controllers_count,
        "summarize: complete"
    );

    Ok(Json(ContextSummarizeResponse {
        success: true,
        summary,
    }))
}

/// Resolve the requested controller mode from the API options.
///
/// Precedence:
/// 1. `controller_mode` (if `Some`) is parsed case-insensitively.
/// 2. Else, `extract_strategy = true` → `Functional` (legacy mapping).
/// 3. Else, `Projection` (default).
///
/// Accepted names: `projection`, `functional`, `permissive`,
/// `signature-memory`, `product-game`, `parity-game`. The dashes may also
/// be underscores or removed (e.g., `parity_game` and `paritygame` work).
fn resolve_controller_mode(
    mode: &Option<String>,
    extract_strategy: bool,
) -> Result<crate::context::ControllerMode, ApiError> {
    use crate::context::ControllerMode;
    if let Some(name) = mode {
        return ControllerMode::from_normalized_name(name).map_err(|other| ApiError::BadRequest {
            message: format!("Unknown controller_mode '{other}'"),
            details: Some(
                "Valid: projection, functional, permissive, signature-memory, product-game, parity-game".into(),
            ),
        });
    }
    Ok(if extract_strategy {
        ControllerMode::Functional
    } else {
        ControllerMode::Projection
    })
}

/// Synthesize controller from ctxdsl specification
pub async fn context_synthesize_handler(
    Json(request): Json<ContextSynthesizeRequest>,
) -> ApiResult<Json<ContextSynthesizeResponse>> {
    blocking(move || context_synthesize_handler_impl(request)).await
}

fn context_synthesize_handler_impl(
    request: ContextSynthesizeRequest,
) -> ApiResult<Json<ContextSynthesizeResponse>> {
    let handler_start = Instant::now();

    // Resolve formula name + synthesized template sidecar BEFORE cache lookup,
    // so the cache key covers the template-ref instantiation. Repeated requests
    // with identical context + identical template params hit the cache.
    let (formula_name, synth_sidecar) = resolve_template_ref_for_cache(
        &request.formula,
        &request.template_ref,
        &request.automaton,
    )?;
    let formula_name = formula_name.ok_or_else(|| ApiError::BadRequest {
        message: "either 'formula' or 'template_ref' must be provided".to_string(),
        details: None,
    })?;

    // Build sidecar string list: original sidecars + (optionally) synthesized template sidecar
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
    info!(
        realize_ms = handler_start.elapsed().as_millis() as u64,
        cache_hit, "synthesize: parse+realize complete"
    );

    // Get formula
    let realized_formula =
        realized
            .formulas
            .get(&formula_name)
            .ok_or_else(|| ApiError::BadRequest {
                message: format!("Unknown formula '{}'", formula_name),
                details: None,
            })?;

    // Verify automaton exists
    if realized.context.clts(&request.automaton).is_none() {
        return Err(ApiError::BadRequest {
            message: format!("Unknown automaton '{}'", request.automaton),
            details: None,
        });
    }

    let env = realized.environment_for(&request.automaton);

    // Build evaluation options
    let eval_options = EvaluationOptions::default();

    // Build diagnostics options
    let mut diagnostics = ContextDiagnosticsOptions::default();
    diagnostics.counterexample = request.options.diagnostics.counterexample;
    diagnostics.deadlock_traces = request.options.diagnostics.deadlock_traces;
    if let Some(max) = request.options.diagnostics.max_counter_traces {
        diagnostics.max_counter_traces = Some(max as usize);
    }

    let diagnostics_ref = Some(diagnostics);

    // Synthesize controller
    let synthesis = realized
        .context
        .synthesise_controller_with_options(
            &request.automaton,
            &realized_formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: Some(&eval_options),
                diagnostics: diagnostics_ref.as_ref(),
                minimize: request.options.minimize,
                extract_strategy: request.options.extract_strategy,
                mode: resolve_controller_mode(
                    &request.options.controller_mode,
                    request.options.extract_strategy,
                )?,
            },
        )
        .map_err(|e| ApiError::Internal {
            message: format!("Controller synthesis failed: {}", e),
            source: None,
        })?;

    // Convert controller to DSL if realizable
    let controller_content = if synthesis.realizable {
        let formula_raw = &realized_formula.raw;
        let content = serialize_controller_to_ctxdsl(
            &synthesis.controller,
            &request.automaton,
            &formula_name,
            formula_raw,
        )?;
        Some(FileContent {
            name: format!("{}_controller.ctxdsl", request.automaton),
            content,
        })
    } else {
        None
    };

    // Compute counterstrategy graph for unrealizable cases
    let counterstrategy = if !synthesis.realizable {
        compute_counterstrategy_result(
            realized,
            &request.automaton,
            &realized_formula.formula,
            &env,
            &eval_options,
            request.options.minimize,
        )
    } else {
        None
    };

    // Convert diagnostics
    let diagnostics = convert_diagnostics(&synthesis.diagnostics);

    // Emit controller in native format if requested
    let controller_native = if synthesis.realizable {
        match request.options.output_format.as_deref() {
            Some("xstate") => {
                use crate::adapter::xstate::emit_controller::controller_to_xstate_json;
                let json =
                    controller_to_xstate_json(&synthesis.controller, &request.automaton, true);
                Some(FileContent {
                    name: format!("{}_controller.json", request.automaton),
                    content: json,
                })
            }
            Some("systemverilog") | Some("sv") => {
                use crate::adapter::systemverilog::emit_controller::controller_to_systemverilog;
                let sv =
                    controller_to_systemverilog(&synthesis.controller, &request.automaton, true);
                Some(FileContent {
                    name: format!("{}_controller.sv", request.automaton),
                    content: sv,
                })
            }
            _ => None,
        }
    } else {
        None
    };

    Ok(Json(ContextSynthesizeResponse {
        success: true,
        realizable: synthesis.realizable,
        controller: controller_content,
        controller_native,
        diagnostics,
        counterstrategy,
    }))
}

/// Sound GR(1) controller synthesis (`POST /api/v1/synth/gr1`). Translates the
/// source to the adapter IR, runs the sound GR(1) synthesizer on the structured
/// LTL spec, and returns the realizability verdict plus (when realizable) the
/// controller SystemVerilog. See `crate::mu_calculus::gr1_build`.
pub async fn gr1_synthesize_handler(
    Json(request): Json<Gr1SynthesizeRequest>,
) -> ApiResult<Json<Gr1SynthesizeResponse>> {
    blocking(move || gr1_synthesize_handler_impl(request)).await
}

fn gr1_synthesize_handler_impl(
    request: Gr1SynthesizeRequest,
) -> ApiResult<Json<Gr1SynthesizeResponse>> {
    let adapter = request.adapter.as_deref().unwrap_or("tlsf");
    if adapter != "tlsf" {
        return Err(ApiError::BadRequest {
            message: format!("GR(1) synthesis currently supports adapter 'tlsf', got '{adapter}'"),
            details: None,
        });
    }
    let ir = crate::adapter::tlsf::translate_to_ir(
        &request.context.content,
        &crate::adapter::AdapterOptions::default(),
    )
    .map_err(|e| ApiError::BadRequest {
        message: format!("TLSF translation failed: {e}"),
        details: None,
    })?;
    let module = request.module.as_deref().unwrap_or("gr1_controller");
    let synth = crate::adapter::gr1_synth::synthesise_gr1_from_ir(&ir, module).map_err(|e| {
        ApiError::BadRequest {
            message: e,
            details: None,
        }
    })?;
    Ok(Json(Gr1SynthesizeResponse {
        realizable: synth.realizable,
        controller_sv: synth.controller_sv,
        game_states: synth.n_game_states,
        monitor_bits: synth.n_monitor_bits,
        notes: synth.notes,
    }))
}

/// Import an external format (XState, SystemVerilog, TLSF, AIGER, Promela) into CTXDSL.
pub async fn context_import_handler(
    Json(request): Json<ContextImportRequest>,
) -> ApiResult<Json<ContextImportResponse>> {
    blocking(move || context_import_handler_impl(request)).await
}

fn context_import_handler_impl(
    request: ContextImportRequest,
) -> ApiResult<Json<ContextImportResponse>> {
    use crate::adapter::{AdapterOptions, FormatAdapter};

    // P3 (§Phase 11 slot-3 close follow-up, 2026-06-12) — thread
    // R-S2b.6 + R-S6.6 path-context options into the bit-blaster
    // (Verilator reset simulation + VCD trace mining
    // orchestrations) AND populate `sidecar_json` so the BTOR2
    // path can consume the sidecar's declarations (which today
    // only the SV adapter reads). Closes the parity gap surfaced
    // by the slot-3 close cadence checkpoint at
    // .claude/reviews/slot-3-close-cadence-2026-06-12.md
    // (R-S2b.6 + R-S6.6 unreachable from API before this wire-in).
    let options = AdapterOptions {
        sidecar_json: request.sidecar.clone(),
        sv_source_path: request
            .sv_source_path
            .as_ref()
            .map(std::path::PathBuf::from),
        sidecar_path: request.sidecar_path.as_ref().map(std::path::PathBuf::from),
        ..AdapterOptions::default()
    };

    let result = match request.format.as_str() {
        "tlsf" => crate::adapter::tlsf::TlsfAdapter::translate(&request.content, &options),
        "aiger" => crate::adapter::aiger::AigerAdapter::translate(&request.content, &options),
        "btor2" | "btor" => {
            crate::adapter::btor2::Btor2Adapter::translate(&request.content, &options)
        }
        "sv-yosys" | "yosys" => {
            // Yosys-driven SV elaboration via child process. Parity with the
            // CLI `--adapter sv-yosys` flag.
            //
            // `use_sv2v` is the API-side mirror of the CLI's
            // `--preprocessor sv2v` flag. When set, the driver runs sv2v
            // as a preprocessing pass before invoking Yosys, lowering
            // modern SystemVerilog constructs (notably module-header
            // `import pkg::*;`) to Verilog-2005 that Yosys handles.
            let yopts = if !request.additional_sources.is_empty() {
                let mut additional = std::collections::HashMap::new();
                for src in &request.additional_sources {
                    additional.insert(src.name.clone(), src.content.clone());
                }
                crate::adapter::yosys::YosysOptions {
                    additional_sources: additional.into_iter().collect(),
                    use_sv2v: request.use_sv2v,
                    ..Default::default()
                }
            } else {
                crate::adapter::yosys::YosysOptions {
                    use_sv2v: request.use_sv2v,
                    ..Default::default()
                }
            };
            crate::adapter::yosys::translate_sv(&request.content, &options, &yopts)
        }
        "promela" => crate::adapter::promela::PromelaAdapter::translate(&request.content, &options),
        "xstate" => crate::adapter::xstate::XStateAdapter::translate(&request.content, &options),
        // The native `systemverilog` / `sv` parser route was removed in
        // S.2b. SystemVerilog is served exclusively by the `sv-yosys`
        // arm above (sv2v → Yosys → BTOR2 → bit-blast).
        "extraction" | "espec" => {
            let mut opts = options.clone();
            opts.mode = Some("vulnerable".to_string());
            crate::adapter::extraction::ExtractionAdapter::translate(&request.content, &opts)
        }
        "crewai" => crate::adapter::crewai::CrewaiAdapter::translate(&request.content, &options),
        "langgraph" => {
            crate::adapter::langgraph::LangGraphAdapter::translate(&request.content, &options)
        }
        "microcode" => {
            crate::adapter::microcode::MicrocodeAdapter::translate(&request.content, &options)
        }
        "auto" | "" => crate::adapter::auto_translate(&request.content, &options),
        other => {
            return Err(ApiError::BadRequest {
                message: format!(
                    "Unknown format '{other}'. Supported: auto, tlsf, aiger, btor2, promela, xstate, sv-yosys, extraction, crewai, langgraph, microcode"
                ),
                details: None,
            });
        }
    };

    let output = result.map_err(|e| ApiError::BadRequest {
        message: format!("Adapter translation failed: {e}"),
        details: None,
    })?;

    let state_valuations = if output.state_valuations.is_empty() {
        None
    } else {
        serde_json::to_value(&output.state_valuations).ok()
    };
    let transition_observations = if output.transition_observations.is_empty() {
        None
    } else {
        serde_json::to_value(&output.transition_observations).ok()
    };

    // R.6.7 / V.6 (2026-06-09) — when the request declares
    // `predicates` AND `controllable_inputs`, post-process the
    // adapter output through `predicate_cube_lift` with the R.6.6
    // controllability-aware dispatch. Today only the `btor2` format
    // is wired through this path (the `sv-yosys` route returns
    // CTXDSL directly; routing it through predicate_cube_lift
    // requires a multi-step plumbing that lifts BTOR2 from the
    // sv2v+Yosys output — queued as a V.6 follow-up).
    //
    // Per CLAUDE.md §Surface Parity, this API surface mirrors the
    // CLI's `--controllable-input` + `--predicate` flags on
    // `mununu btor2 cegar`. The UI consumer (mununu-ui) renders
    // the lift summary in its dedicated workflow page.
    let is_btor2_format = matches!(request.format.as_str(), "btor2" | "btor");
    if !request.predicates.is_empty() && !request.controllable_inputs.is_empty() && is_btor2_format
    {
        return run_controllability_aware_lift(&request);
    }

    Ok(Json(ContextImportResponse {
        success: true,
        ctxdsl: output.ctxdsl,
        source_format: output.source_info.format.to_string(),
        warnings: output.warnings.iter().map(|w| w.message.clone()).collect(),
        signal_count: output.source_info.signal_count,
        state_count: output.source_info.state_count,
        property_count: output.source_info.property_count,
        state_valuations,
        transition_observations,
    }))
}

/// R.6.7 / V.6 (2026-06-09) — controllability-aware predicate-cube
/// lift entry point. Invoked from `context_import_handler` when the
/// request declares both `predicates` + `controllable_inputs`.
///
/// Today: BTOR2 input only. Returns a `ContextImportResponse` whose
/// `warnings` list captures the lift's `AdapterWarning`s + a
/// summary line counting `MayOnly` / `Sharp` / `MustHyperOnly`
/// transitions for UI rendering. The `ctxdsl` field carries a
/// human-readable summary line + the warnings as comments — full
/// CTXDSL emit from a `Clts` is a follow-up (the existing
/// `adapter::emit::emit` takes an `AdapterIR`, not a `Clts`).
fn run_controllability_aware_lift(
    request: &ContextImportRequest,
) -> ApiResult<Json<ContextImportResponse>> {
    use crate::adapter::AdapterOptions;
    use crate::adapter::btor2::kmts_lift::{
        MustEdgeInference, PredicateCubeLiftOptions, PredicateSpec, predicate_cube_lift,
    };
    use crate::clts::TransitionModality;

    let predicates: Vec<PredicateSpec> = request
        .predicates
        .iter()
        .map(|p| PredicateSpec {
            name: p.name.clone(),
            register: p.register.clone(),
            value: p.value,
        })
        .collect();

    let adapter_options = AdapterOptions {
        controllable_inputs: request.controllable_inputs.clone(),
        ..Default::default()
    };

    let lift_opts = PredicateCubeLiftOptions {
        // Reasonable defaults for an interactive UI workflow.
        max_cube_count: 1024,
        max_input_bits: 8,
        must_edge_inference: MustEdgeInference::Off,
        may_edge_inference: Default::default(),
        config_values: std::collections::HashMap::new(),
        compound_exprs: std::collections::HashMap::new(),
        derived_predicates: Vec::new(),
        may_postimage: false,
    };

    let lift_result =
        predicate_cube_lift(predicates, &request.content, &adapter_options, &lift_opts).map_err(
            |e| ApiError::BadRequest {
                message: format!("controllability-aware lift failed: {}", e.message),
                details: None,
            },
        )?;

    let mut mayonly = 0usize;
    let mut sharp = 0usize;
    let mut hyper_must = 0usize;
    for state in lift_result.clts.states() {
        for trans in lift_result.clts.outgoing(state) {
            match trans.modality() {
                TransitionModality::MayOnly => mayonly += 1,
                TransitionModality::Sharp => sharp += 1,
                TransitionModality::MustHyperOnly(_) => hyper_must += 1,
            }
        }
    }

    let alphabet = lift_result.clts.alphabet();
    let env_label_count = alphabet.iter().filter(|l| l.starts_with("env_c")).count();
    let ctrl_label_count = alphabet.iter().filter(|l| l.starts_with("ctrl_c")).count();

    let mut warnings: Vec<String> = lift_result
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();
    warnings.push(format!(
        "[R.6.7 V.6 controllability-aware lift] cubes={}, mayonly={}, sharp={}, hyper_must={}, env_labels={}, ctrl_labels={}",
        lift_result.cube_count, mayonly, sharp, hyper_must, env_label_count, ctrl_label_count
    ));

    // Summary CTXDSL — the full Clts→CTXDSL emit is queued as a
    // follow-up. For the V.6 UI MVP we return a comment-only CTXDSL
    // carrying the lift summary, so the UI's Monaco editor +
    // existing graph view degrade gracefully (the warnings list +
    // numeric fields are the canonical summary).
    let summary_ctxdsl = format!(
        "// R.6.7 / V.6 controllability-aware lift summary.\n\
         // cubes={}\n// mayonly={}\n// sharp={}\n// hyper_must={}\n\
         // env_labels={} (Uncontrollable)\n// ctrl_labels={} (Controllable)\n\
         // Full CTXDSL emit from the lifted KMTS is a follow-up;\n\
         // the structured lift fields (state_count, warnings) are\n\
         // the canonical summary for now.\n\
         context v6_controllability_kmts_summary {{\n\
         }}\n",
        lift_result.cube_count, mayonly, sharp, hyper_must, env_label_count, ctrl_label_count
    );

    Ok(Json(ContextImportResponse {
        success: true,
        ctxdsl: summary_ctxdsl,
        source_format: format!("{:?}", lift_result.source_info.format).to_lowercase(),
        warnings,
        signal_count: lift_result.source_info.signal_count,
        state_count: lift_result.cube_count,
        property_count: 0,
        state_valuations: None,
        transition_observations: None,
    }))
}

/// U.0 (slot 6) — run the CEGAR predicate-abstraction-refinement loop over
/// a BTOR2 design and return the per-iteration refinement trace + the final
/// 3-valued verdict. The API equivalent of `mununu btor2 cegar`, exposed so
/// the UI refinement-trace viewer can render the refinement story (which
/// predicates were added at each iteration, where KleeneBot drove a split,
/// and why the loop terminated). Heavy (Z3); UI calls it via the extended
/// timeout client.
pub async fn btor2_cegar_handler(
    Json(request): Json<Btor2CegarRequest>,
) -> ApiResult<Json<Btor2CegarResponse>> {
    blocking(move || btor2_cegar_handler_impl(request)).await
}

fn btor2_cegar_handler_impl(request: Btor2CegarRequest) -> ApiResult<Json<Btor2CegarResponse>> {
    if request.predicates.is_empty() {
        return Err(ApiError::BadRequest {
            message: "at least one predicate is required to bootstrap the cube space".to_string(),
            details: None,
        });
    }
    let params = CegarRunParams {
        btor2_content: &request.content,
        formula: &request.formula,
        predicates: &request.predicates,
        controllable_inputs: &request.controllable_inputs,
        predicate_source: request.predicate_source.as_deref(),
        max_iterations: request.max_iterations,
        must_edge_inference: request.must_edge_inference.as_deref(),
        may_edge_inference: request.may_edge_inference.as_deref(),
        config_values: &request.config_values,
        emit_ctxdsl: request.emit_ctxdsl,
        engine: request.engine.as_deref(),
    };
    Ok(Json(run_cegar_build_response(params)?))
}

/// Decide `bad`-reachability of a BTOR2 design with the multi-engine safety
/// portfolio (`POST /api/v1/btor2/verify`). Surface peer of the CLI
/// `mununu btor2 verify`: runs every available sound engine (exact ⊕ native ⊕
/// spacer ⊕ btormc ⊕ Pono) via the **parallel** driver — its wall-clock is bounded
/// by the slowest single engine, keeping the request within the extended client's
/// budget even when the subprocess members are present — and returns the merged
/// verdict + per-engine breakdown. A `"contradiction"` verdict means two sound
/// engines disagree (a soundness alarm), never a silent guess.
pub async fn btor2_verify_handler(
    Json(request): Json<Btor2VerifyRequest>,
) -> ApiResult<Json<Btor2VerifyResponse>> {
    blocking(move || btor2_verify_handler_impl(request)).await
}

fn btor2_verify_handler_impl(request: Btor2VerifyRequest) -> ApiResult<Json<Btor2VerifyResponse>> {
    use crate::adapter::reach_portfolio::{ReachVerdict, decide_reach_portfolio_parallel};

    let file = crate::adapter::btor2::parser::parse(&request.content).map_err(|e| {
        ApiError::BadRequest {
            message: format!("BTOR2 parse error: {e}"),
            details: None,
        }
    })?;

    let outcome = decide_reach_portfolio_parallel(&file);
    Ok(Json(Btor2VerifyResponse {
        verdict: crate::verdict::PropertyVerdict::from(outcome.verdict)
            .as_str()
            .to_string(),
        reachable_by: outcome.reachable_by.iter().map(|s| s.to_string()).collect(),
        unreachable_by: outcome
            .unreachable_by
            .iter()
            .map(|s| s.to_string())
            .collect(),
        contradiction: outcome.verdict == ReachVerdict::Contradiction,
    }))
}

/// Decide the response-liveness property `AG(request → AF grant)` at scale
/// (`POST /api/v1/btor2/verify-liveness`). Surface peer of the CLI
/// `mununu btor2 verify-liveness`: reduces the property to a single
/// `bad`-reachability query (Biere–Artho–Schuppan liveness-to-safety) the portfolio
/// decides, returning the canonical `"holds"` / `"violated"` / `"unknown"` verdict.
/// A malformed atom or unparseable BTOR2 is a `BadRequest`.
pub async fn btor2_verify_liveness_handler(
    Json(request): Json<Btor2VerifyLivenessRequest>,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    blocking(move || btor2_verify_liveness_handler_impl(request)).await
}

fn btor2_verify_liveness_handler_impl(
    request: Btor2VerifyLivenessRequest,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    use crate::adapter::liveness_rescue::{parse_response_atom, response_liveness_rescue_atoms};

    let bad_req = |message: String| ApiError::BadRequest {
        message,
        details: None,
    };
    let ante = parse_response_atom(&request.request).map_err(bad_req)?;
    let cons = parse_response_atom(&request.grant).map_err(bad_req)?;

    let (verdict, outcome) = response_liveness_rescue_atoms(&request.content, &ante, &cons, false)
        .ok_or_else(|| {
            bad_req(
            "could not build the liveness monitor — an atom likely binds no signal in the design"
                .to_string(),
        )
        })?;

    Ok(Json(Btor2VerifyLivenessResponse {
        verdict: crate::verdict::PropertyVerdict::from(verdict)
            .as_str()
            .to_string(),
        property: format!("AG(({}) -> AF ({}))", request.request, request.grant),
        decided_by: outcome
            .reachable_by
            .iter()
            .chain(outcome.unreachable_by.iter())
            .map(|s| s.to_string())
            .collect(),
    }))
}

/// Decide the conjunction of response-liveness properties `⋀ᵢ AG(aᵢ → AF bᵢ)`
/// (`POST /api/v1/btor2/verify-liveness-all`). Surface peer of the CLI
/// `mununu btor2 verify-liveness-all`: reduces each `"ANTE => CONS"` conjunct to its
/// own `bad`-reachability query, returning the combined `"holds"` / `"violated"` /
/// `"unknown"` verdict. A malformed response (missing `=>` or a non-atom side) or
/// unparseable BTOR2 is a `BadRequest`.
pub async fn btor2_verify_liveness_all_handler(
    Json(request): Json<Btor2VerifyLivenessAllRequest>,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    blocking(move || btor2_verify_liveness_all_handler_impl(request)).await
}

fn btor2_verify_liveness_all_handler_impl(
    request: Btor2VerifyLivenessAllRequest,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    use crate::adapter::liveness_rescue::{
        parse_response_pairs, response_conjunction_property, response_liveness_rescue_conjunction,
    };

    let bad_req = |message: String| ApiError::BadRequest {
        message,
        details: None,
    };
    let pairs = parse_response_pairs(&request.responses).map_err(bad_req)?;
    let property = response_conjunction_property(&request.responses);

    let (verdict, outcomes) = response_liveness_rescue_conjunction(&request.content, &pairs, false)
        .ok_or_else(|| {
            bad_req(
                "could not build a liveness monitor — an atom likely binds no signal, or no \
                 responses were given"
                    .to_string(),
            )
        })?;

    Ok(Json(Btor2VerifyLivenessResponse {
        verdict: crate::verdict::PropertyVerdict::from(verdict)
            .as_str()
            .to_string(),
        property,
        decided_by: outcomes
            .iter()
            .flat_map(|o| o.reachable_by.iter().chain(o.unreachable_by.iter()))
            .map(|s| s.to_string())
            .collect(),
    }))
}

/// mununu#477 Option B — decide `(⋀ⱼ GF fairⱼ) → AG(request → AF grant)`
/// (`POST /api/v1/btor2/verify-liveness-under-fairness`) via the Emerson–Lei
/// fair-cycle l2s + the reachability portfolio. Surface peer of the CLI
/// `mununu btor2 verify-liveness-under-fairness`. A malformed atom is a
/// `BadRequest`; empty `fairness` recovers `verify-liveness` exactly.
pub async fn btor2_verify_liveness_under_fairness_handler(
    Json(request): Json<Btor2VerifyLivenessUnderFairnessRequest>,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    blocking(move || btor2_verify_liveness_under_fairness_handler_impl(request)).await
}

fn btor2_verify_liveness_under_fairness_handler_impl(
    request: Btor2VerifyLivenessUnderFairnessRequest,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    use crate::adapter::liveness_rescue::{
        parse_fairness_atoms, parse_response_atom, response_liveness_rescue_under_fairness,
    };

    let bad_req = |message: String| ApiError::BadRequest {
        message,
        details: None,
    };
    let ante = parse_response_atom(&request.request).map_err(bad_req)?;
    let cons = parse_response_atom(&request.grant).map_err(bad_req)?;
    let fairness = parse_fairness_atoms(&request.fairness).map_err(bad_req)?;

    let (verdict, outcome) =
        response_liveness_rescue_under_fairness(&request.content, &ante, &cons, &fairness, false)
            .ok_or_else(|| {
            bad_req(
            "could not build the fair-cycle liveness monitor — an atom likely binds no signal in \
             the design"
                .to_string(),
        )
        })?;

    let property = if request.fairness.is_empty() {
        format!("AG(({}) -> AF ({}))", request.request, request.grant)
    } else {
        let fair_and = request
            .fairness
            .iter()
            .map(|f| format!("GF ({f})"))
            .collect::<Vec<_>>()
            .join(" && ");
        format!(
            "({fair_and}) -> AG(({}) -> AF ({}))",
            request.request, request.grant
        )
    };

    Ok(Json(Btor2VerifyLivenessResponse {
        verdict: crate::verdict::PropertyVerdict::from(verdict)
            .as_str()
            .to_string(),
        property,
        decided_by: outcome
            .reachable_by
            .iter()
            .chain(outcome.unreachable_by.iter())
            .map(|s| s.to_string())
            .collect(),
    }))
}

/// Decide recoverability `AG EF target` (`POST /api/v1/btor2/verify-recoverability`)
/// — "from every reachable state, can the design get back to `target`?", the
/// branching property SVA cannot state. Surface peer of the CLI
/// `mununu btor2 verify-recoverability`: decides it with the exact 3-valued engine
/// (sound at every alternation depth; `"unknown"` over the engine's cap). A malformed
/// target atom is a `BadRequest`.
pub async fn btor2_verify_recoverability_handler(
    Json(request): Json<Btor2VerifyRecoverabilityRequest>,
) -> ApiResult<Json<Btor2VerifyRecoverabilityResponse>> {
    blocking(move || btor2_verify_recoverability_handler_impl(request)).await
}

fn btor2_verify_recoverability_handler_impl(
    request: Btor2VerifyRecoverabilityRequest,
) -> ApiResult<Json<Btor2VerifyRecoverabilityResponse>> {
    use crate::adapter::recoverability::{
        parse_config_value_specs, parse_extra_predicate, recoverability_property_str,
        verify_recoverability_refined, verify_recoverability_with_predicates,
    };

    let extra = request
        .predicates
        .iter()
        .map(|s| parse_extra_predicate(s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| ApiError::BadRequest {
            message,
            details: None,
        })?;
    let config_specs = parse_config_value_specs(&request.config_values).map_err(|message| {
        ApiError::BadRequest {
            message,
            details: None,
        }
    })?;
    // `refine`/`config_values`/`discover_assumptions` (refined-verdicts): canonical verdict PLUS a
    // diagnostic-only refinement.
    let (verdict, refinement) =
        if request.refine || !config_specs.is_empty() || request.discover_assumptions {
            let (v, r) = verify_recoverability_refined(
                &request.content,
                &request.target,
                &extra,
                &config_specs,
                request.discover_assumptions,
            );
            (v, Some(r))
        } else {
            let v =
                verify_recoverability_with_predicates(&request.content, &request.target, &extra)
                    .map_err(|message| ApiError::BadRequest {
                        message,
                        details: None,
                    })?;
            (v, None)
        };

    Ok(Json(Btor2VerifyRecoverabilityResponse {
        verdict: verdict.as_str().to_string(),
        property: recoverability_property_str(&request.target),
        refinement,
    }))
}

/// Auto-scan every FSM-like state register for a reachable illegal encoding
/// (`POST /api/v1/btor2/check-fsm`) — no user input. Surface peer of the CLI
/// `mununu btor2 check-fsm`: derives each narrow state register's legal encoding set
/// from the design and checks, from the reset state, whether any illegal encoding is
/// reachable via the word-level portfolio; a `"violated"` register has a reachable
/// illegal encoding (a bug). A malformed BTOR2 source is a `BadRequest`.
pub async fn btor2_check_fsm_handler(
    Json(request): Json<Btor2CheckFsmRequest>,
) -> ApiResult<Json<Btor2CheckFsmResponse>> {
    blocking(move || btor2_check_fsm_handler_impl(request)).await
}

fn btor2_check_fsm_handler_impl(
    request: Btor2CheckFsmRequest,
) -> ApiResult<Json<Btor2CheckFsmResponse>> {
    use crate::adapter::fsm_scan::fsm_encoding_scan;

    let findings = fsm_encoding_scan(&request.content, request.max_width).map_err(|message| {
        ApiError::BadRequest {
            message,
            details: None,
        }
    })?;

    let illegal_encodings_found = findings.iter().filter(|f| f.is_finding()).count();
    let registers = findings
        .iter()
        .map(|f| FsmRegisterFinding {
            register: f.register.clone(),
            legal_encodings: f.legal_encodings.clone(),
            verdict: f.verdict.as_str().to_string(),
            illegal_encoding_reachable: f.is_finding(),
        })
        .collect();

    Ok(Json(Btor2CheckFsmResponse {
        fsm_registers_checked: findings.len(),
        illegal_encodings_found,
        registers,
    }))
}

/// Solve the two-player controllable-reachability game (`POST /api/v1/btor2/game`) and synthesize the
/// winner's strategy. Surface peer of the CLI `mununu btor2 game`: partitions the primary inputs into
/// controller-owned (`controllable`) vs environment-owned (the adversary), decides whether the controller
/// can force the design to `good` against every environment move, and returns `realizable` plus the
/// controller's Mealy strategy — or, when unrealizable, the environment's positional counterstrategy (the
/// witness for why no controller works, motivating an assume-guarantee assumption). A malformed BTOR2
/// source, an unresolvable `good` atom, or a `controllable` name that is not a primary input is a
/// `BadRequest`.
pub async fn btor2_game_handler(
    Json(request): Json<Btor2GameRequest>,
) -> ApiResult<Json<Btor2GameResponse>> {
    blocking(move || btor2_game_handler_impl(request)).await
}

fn btor2_game_handler_impl(request: Btor2GameRequest) -> ApiResult<Json<Btor2GameResponse>> {
    use crate::adapter::btor2::symbolic_bitblast::{
        exact_two_player_buchi_realizable, exact_two_player_buchi_strategy,
        exact_two_player_reach_realizable, exact_two_player_recurrence_stall_lasso,
        exact_two_player_strategy, game_sound_posture_model,
    };
    use crate::api::models::GameObjective;

    // assume_clock_reset: model clk/rst as a sound posture (not adversarial) before solving.
    let content = if request.assume_clock_reset {
        game_sound_posture_model(&request.content)
    } else {
        request.content.clone()
    };
    let controllable: Vec<&str> = request.controllable.iter().map(String::as_str).collect();
    let recurrence = request.objective == GameObjective::Recurrence;
    // The VERDICT works on any resolvable target (state cell, combinational output, relation) for both
    // objectives and validates the partition. `reach` = force `good` once; `recurrence` = force `good`
    // infinitely often (Büchi).
    let realizable = if recurrence {
        exact_two_player_buchi_realizable(&content, &request.good, &controllable)
    } else {
        exact_two_player_reach_realizable(&content, &request.good, &controllable)
    }
    .map_err(|message| ApiError::BadRequest {
        message,
        details: None,
    })?;
    // STRATEGY extraction — the WINNER's strategy, best-effort (needs a STATE-register target, so it is
    // `null` for a combinational-output / relational `good` like a FIFO's `full_o`). `reach` = the
    // reachability attractor strategy (controller Mealy, or the env positional counterstrategy when
    // unrealizable); `recurrence` = the CONTROLLER's Büchi strategy when realizable (`null` when
    // unrealizable — the `stall_lasso` below is that case's witness).
    let strategy = if recurrence {
        exact_two_player_buchi_strategy(&content, &request.good, &controllable)
            .ok()
            .flatten()
    } else {
        exact_two_player_strategy(&content, &request.good, &controllable).ok()
    };
    // Assumption discovery: for `reach` a SAFETY hold / conjunction (`A ⇒ G`); for `recurrence` a
    // FAIRNESS assumption `GF a → GF good` (the GR(1) 1-pair objective).
    let holds_under = if request.discover_assumptions {
        if recurrence {
            crate::adapter::recoverability::discover_game_fairness_assumption(
                &content,
                &request.good,
                &controllable,
            )
        } else {
            crate::adapter::recoverability::discover_game_env_assumption(
                &content,
                &request.good,
                &controllable,
            )
        }
    } else {
        Vec::new()
    };
    // ENVIRONMENT STARVATION LASSO — the actionable witness for an unrealizable RECURRENCE game (the
    // Büchi analog of the reach counterstrategy). Best-effort; `null` when there is no simple reachable
    // force-`¬good`-forever region. A malformed model here would already have failed the verdict above.
    let stall_lasso = if recurrence && !realizable {
        exact_two_player_recurrence_stall_lasso(&content, &request.good, &controllable)
            .ok()
            .flatten()
            .map(|l| l.to_view())
    } else {
        None
    };

    Ok(Json(Btor2GameResponse {
        realizable,
        good: request.good,
        objective: request.objective,
        controllable: request.controllable,
        holds_under,
        strategy,
        stall_lasso,
    }))
}

// --- SV-direct verbs: lift SV (sv2v + Yosys) then decide, in one call. Surface peers
// of the CLI `sv verify` / `verify-liveness` / `verify-recoverability`; they return
// the same `Btor2Verify*Response` shapes as the BTOR2-direct verbs. The lift needs
// sv2v + Yosys on the host (a missing tool → BadRequest). ---

/// `POST /api/v1/sv/verify` — lift SV and decide `bad`-reachability of its assertions
/// with the multi-engine safety portfolio.
pub async fn sv_verify_handler(
    Json(request): Json<SvVerifyRequest>,
) -> ApiResult<Json<Btor2VerifyResponse>> {
    blocking(move || sv_verify_handler_impl(request)).await
}

fn sv_verify_handler_impl(request: SvVerifyRequest) -> ApiResult<Json<Btor2VerifyResponse>> {
    use crate::adapter::reach_portfolio::ReachVerdict;
    use crate::adapter::sv_verify::{SvLift, sv_verify_safety};

    let lift = SvLift {
        source: request.source,
        additional_sources: request
            .additional_sources
            .into_iter()
            .map(|f| (f.name, f.content))
            .collect(),
        top: request.top,
        use_sv2v: request.use_sv2v,
        // No API analog: the request carries source *content* by name, so an
        // on-disk include-search dir is a local (CLI) concept; the flat
        // name-staging of `additional_sources` already resolves includes here.
        include_dirs: Vec::new(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
    };
    let outcome = sv_verify_safety(&lift).map_err(|message| ApiError::BadRequest {
        message,
        details: None,
    })?;
    Ok(Json(Btor2VerifyResponse {
        verdict: crate::verdict::PropertyVerdict::from(outcome.verdict)
            .as_str()
            .to_string(),
        reachable_by: outcome.reachable_by.iter().map(|s| s.to_string()).collect(),
        unreachable_by: outcome
            .unreachable_by
            .iter()
            .map(|s| s.to_string())
            .collect(),
        contradiction: outcome.verdict == ReachVerdict::Contradiction,
    }))
}

/// `POST /api/v1/sv/verify-liveness` — lift SV and decide `AG(request → AF grant)`.
pub async fn sv_verify_liveness_handler(
    Json(request): Json<SvVerifyLivenessRequest>,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    blocking(move || sv_verify_liveness_handler_impl(request)).await
}

fn sv_verify_liveness_handler_impl(
    request: SvVerifyLivenessRequest,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    use crate::adapter::sv_verify::{SvLift, sv_verify_liveness};

    let property = format!("AG(({}) -> AF ({}))", request.request, request.grant);
    let lift = SvLift {
        source: request.source,
        additional_sources: request
            .additional_sources
            .into_iter()
            .map(|f| (f.name, f.content))
            .collect(),
        top: request.top,
        use_sv2v: request.use_sv2v,
        // No API analog: the request carries source *content* by name, so an
        // on-disk include-search dir is a local (CLI) concept; the flat
        // name-staging of `additional_sources` already resolves includes here.
        include_dirs: Vec::new(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
    };
    let (verdict, outcome) =
        sv_verify_liveness(&lift, &request.request, &request.grant).map_err(|message| {
            ApiError::BadRequest {
                message,
                details: None,
            }
        })?;
    Ok(Json(Btor2VerifyLivenessResponse {
        verdict: crate::verdict::PropertyVerdict::from(verdict)
            .as_str()
            .to_string(),
        property,
        decided_by: outcome
            .reachable_by
            .iter()
            .chain(outcome.unreachable_by.iter())
            .map(|s| s.to_string())
            .collect(),
    }))
}

/// `POST /api/v1/sv/verify-liveness-all` — lift SV and decide the conjunction
/// `⋀ᵢ AG(aᵢ → AF bᵢ)` from `"ANTE => CONS"` response pairs. SV-direct peer of the
/// CLI `sv verify-liveness-all`; reuses the [`Btor2VerifyLivenessResponse`] shape.
pub async fn sv_verify_liveness_all_handler(
    Json(request): Json<SvVerifyLivenessAllRequest>,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    blocking(move || sv_verify_liveness_all_handler_impl(request)).await
}

fn sv_verify_liveness_all_handler_impl(
    request: SvVerifyLivenessAllRequest,
) -> ApiResult<Json<Btor2VerifyLivenessResponse>> {
    use crate::adapter::liveness_rescue::response_conjunction_property;
    use crate::adapter::sv_verify::{SvLift, sv_verify_liveness_all};

    let property = response_conjunction_property(&request.responses);
    let lift = SvLift {
        source: request.source,
        additional_sources: request
            .additional_sources
            .into_iter()
            .map(|f| (f.name, f.content))
            .collect(),
        top: request.top,
        use_sv2v: request.use_sv2v,
        // No API analog: the request carries source *content* by name, so an
        // on-disk include-search dir is a local (CLI) concept; the flat
        // name-staging of `additional_sources` already resolves includes here.
        include_dirs: Vec::new(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
    };
    let (verdict, outcomes) =
        sv_verify_liveness_all(&lift, &request.responses).map_err(|message| {
            ApiError::BadRequest {
                message,
                details: None,
            }
        })?;
    Ok(Json(Btor2VerifyLivenessResponse {
        verdict: crate::verdict::PropertyVerdict::from(verdict)
            .as_str()
            .to_string(),
        property,
        decided_by: outcomes
            .iter()
            .flat_map(|o| o.reachable_by.iter().chain(o.unreachable_by.iter()))
            .map(|s| s.to_string())
            .collect(),
    }))
}

/// `POST /api/v1/sv/verify-recoverability` — lift SV and decide `AG EF target`.
pub async fn sv_verify_recoverability_handler(
    Json(request): Json<SvVerifyRecoverabilityRequest>,
) -> ApiResult<Json<Btor2VerifyRecoverabilityResponse>> {
    blocking(move || sv_verify_recoverability_handler_impl(request)).await
}

fn sv_verify_recoverability_handler_impl(
    request: SvVerifyRecoverabilityRequest,
) -> ApiResult<Json<Btor2VerifyRecoverabilityResponse>> {
    use crate::adapter::recoverability::{
        parse_config_value_specs, parse_extra_predicate, recoverability_property_str,
    };
    use crate::adapter::sv_verify::{
        SvLift, sv_verify_recoverability_refined, sv_verify_recoverability_with_predicates,
    };

    let property = recoverability_property_str(&request.target);
    let extra = request
        .predicates
        .iter()
        .map(|s| parse_extra_predicate(s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| ApiError::BadRequest {
            message,
            details: None,
        })?;
    let config_specs = parse_config_value_specs(&request.config_values).map_err(|message| {
        ApiError::BadRequest {
            message,
            details: None,
        }
    })?;
    let lift = SvLift {
        source: request.source,
        additional_sources: request
            .additional_sources
            .into_iter()
            .map(|f| (f.name, f.content))
            .collect(),
        top: request.top,
        use_sv2v: request.use_sv2v,
        // No API analog: the request carries source *content* by name, so an
        // on-disk include-search dir is a local (CLI) concept; the flat
        // name-staging of `additional_sources` already resolves includes here.
        include_dirs: Vec::new(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
    };
    let (verdict, refinement) =
        if request.refine || !config_specs.is_empty() || request.discover_assumptions {
            let (v, r) = sv_verify_recoverability_refined(
                &lift,
                &request.target,
                &extra,
                &config_specs,
                request.discover_assumptions,
            )
            .map_err(|message| ApiError::BadRequest {
                message,
                details: None,
            })?;
            (v, Some(r))
        } else {
            let v = sv_verify_recoverability_with_predicates(&lift, &request.target, &extra)
                .map_err(|message| ApiError::BadRequest {
                    message,
                    details: None,
                })?;
            (v, None)
        };
    Ok(Json(Btor2VerifyRecoverabilityResponse {
        verdict: verdict.as_str().to_string(),
        property,
        refinement,
    }))
}

/// `POST /api/v1/sv/check-fsm` — lift SV and auto-scan every FSM register for a
/// reachable illegal encoding. Surface peer of the CLI `sv check-fsm`; returns the same
/// [`Btor2CheckFsmResponse`] as the BTOR2-direct verb.
pub async fn sv_check_fsm_handler(
    Json(request): Json<SvCheckFsmRequest>,
) -> ApiResult<Json<Btor2CheckFsmResponse>> {
    blocking(move || sv_check_fsm_handler_impl(request)).await
}

fn sv_check_fsm_handler_impl(request: SvCheckFsmRequest) -> ApiResult<Json<Btor2CheckFsmResponse>> {
    use crate::adapter::sv_verify::{SvLift, sv_check_fsm};

    let lift = SvLift {
        source: request.source,
        additional_sources: request
            .additional_sources
            .into_iter()
            .map(|f| (f.name, f.content))
            .collect(),
        top: request.top,
        use_sv2v: request.use_sv2v,
        // No API analog: the request carries source *content* by name, so an
        // on-disk include-search dir is a local (CLI) concept; the flat
        // name-staging of `additional_sources` already resolves includes here.
        include_dirs: Vec::new(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
    };
    let findings =
        sv_check_fsm(&lift, request.max_width).map_err(|message| ApiError::BadRequest {
            message,
            details: None,
        })?;

    let illegal_encodings_found = findings.iter().filter(|f| f.is_finding()).count();
    let registers = findings
        .iter()
        .map(|f| FsmRegisterFinding {
            register: f.register.clone(),
            legal_encodings: f.legal_encodings.clone(),
            verdict: f.verdict.as_str().to_string(),
            illegal_encoding_reachable: f.is_finding(),
        })
        .collect();

    Ok(Json(Btor2CheckFsmResponse {
        fsm_registers_checked: findings.len(),
        illegal_encodings_found,
        registers,
    }))
}

/// monono#partsel preflight (#469) — lift SV and report the partial-write
/// registers the verifier cannot keep faithfully, with no model checking. Surface
/// peer of the CLI `mununu sv lint`; read-only, changes no verdict.
pub async fn sv_lint_handler(
    Json(request): Json<SvLintRequest>,
) -> ApiResult<Json<SvLintResponse>> {
    blocking(move || sv_lint_handler_impl(request)).await
}

fn sv_lint_handler_impl(request: SvLintRequest) -> ApiResult<Json<SvLintResponse>> {
    use crate::adapter::sv_verify::{SvLift, SvLintSignalKind, sv_lint_registers};

    let lift = SvLift {
        source: request.source,
        additional_sources: request
            .additional_sources
            .into_iter()
            .map(|f| (f.name, f.content))
            .collect(),
        top: request.top,
        use_sv2v: request.use_sv2v,
        // As with the other SV-direct handlers: the request stages include content
        // by name, so an on-disk include-search dir is a CLI-only concept here.
        include_dirs: Vec::new(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
    };

    let findings = sv_lint_registers(&lift).map_err(|message| ApiError::BadRequest {
        message,
        details: None,
    })?;

    let registers_flagged = findings
        .iter()
        .filter(|f| f.kind == SvLintSignalKind::Register)
        .count();
    let findings = findings
        .into_iter()
        .map(|f| SvLintFinding {
            signal: f.signal,
            kind: f.kind.as_str().to_string(),
            rule: f.rule.as_str().to_string(),
            detail: f.detail,
        })
        .collect::<Vec<_>>();

    Ok(Json(SvLintResponse {
        signals_flagged: findings.len(),
        registers_flagged,
        findings,
    }))
}

/// #468 — apply a NAMED structural mutation and re-verify to check the property
/// verdicts FLIP (property adequacy), or (with `list`) enumerate the available
/// mutation targets. Surface peer of the CLI `mununu sv mutate`.
pub async fn sv_mutate_handler(
    Json(request): Json<SvMutateRequest>,
) -> ApiResult<Json<SvMutateResponse>> {
    blocking(move || sv_mutate_handler_impl(request)).await
}

fn sv_mutate_handler_impl(request: SvMutateRequest) -> ApiResult<Json<SvMutateResponse>> {
    use crate::adapter::btor2::kmts_lift::MustEdgeInference;
    use crate::adapter::btor2::mutate::{Mutation, list_targets};
    use crate::adapter::slang::verify_auto::{
        VerifyAutoOptions, engine_selection, mutate_and_compare,
    };
    use crate::adapter::yosys::{SvFrontend, YosysOptions, sv_to_btor2};

    let mut sources: Vec<(String, String)> = vec![("top.sv".to_string(), request.source.clone())];
    for f in &request.additional_sources {
        sources.push((f.name.clone(), f.content.clone()));
    }
    // mununu#475 item 4 — --param API parity. Same parse shape as the CLI
    // + as `sv verify-auto`'s params: `NAME=VALUE` or `MODULE.NAME=VALUE`,
    // both sides non-empty. A malformed entry is a HARD BadRequest here;
    // yosys errors downstream on a parameter it cannot apply.
    let params: Vec<(String, String)> = request
        .params
        .iter()
        .map(|e| {
            let (lhs, val) = e.split_once('=').ok_or_else(|| ApiError::BadRequest {
                message: format!(
                    "malformed `params` entry '{e}': expected NAME=VALUE or MODULE.NAME=VALUE"
                ),
                details: None,
            })?;
            let (lhs, val) = (lhs.trim(), val.trim());
            if lhs.is_empty() || val.is_empty() {
                return Err(ApiError::BadRequest {
                    message: format!(
                        "malformed `params` entry '{e}': NAME and VALUE must both be non-empty"
                    ),
                    details: None,
                });
            }
            Ok((lhs.to_string(), val.to_string()))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let yopts = YosysOptions {
        top: request.top.clone(),
        additional_sources: request
            .additional_sources
            .iter()
            .map(|f| (f.name.clone(), f.content.clone()))
            .collect(),
        use_sv2v: request.use_sv2v,
        frontend: if request.use_slang {
            SvFrontend::Slang
        } else {
            SvFrontend::Auto
        },
        params,
        ..Default::default()
    };

    // `list` mode — lift once and enumerate the targets (no verification).
    if request.list {
        let btor2 = sv_to_btor2(&request.source, &yopts).map_err(|e| ApiError::BadRequest {
            message: format!("sv mutate --list: {}", e.message),
            details: None,
        })?;
        let t = list_targets(&btor2).map_err(|message| ApiError::BadRequest {
            message,
            details: None,
        })?;
        return Ok(Json(SvMutateResponse {
            mutation: None,
            targets: Some(SvMutateTargets {
                stick: t.stick,
                drop_reset: t.drop_reset,
                off_by_one: t.off_by_one,
                invert_cond: t.invert_cond,
            }),
            properties: Vec::new(),
            flipped: 0,
            unflipped: 0,
        }));
    }

    let mutation_spec = request
        .mutation
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest {
            message: "sv mutate: `mutation` is required (or set `list` to discover targets)"
                .to_string(),
            details: Some("expected \"stick:<reg>\" or \"drop-reset:<reg>\"".into()),
        })?;
    let mutation = Mutation::parse(mutation_spec).map_err(|message| ApiError::BadRequest {
        message,
        details: None,
    })?;

    let must_edge_inference = match request.must_edge_inference.as_deref() {
        Some("smt-per-target") => MustEdgeInference::SmtPerTarget,
        Some("smt-per-target-standard") => MustEdgeInference::SmtPerTargetStandard,
        Some("smt-hyper-must") => MustEdgeInference::SmtHyperMust,
        _ => MustEdgeInference::Off,
    };
    let (engine_symbolic, engine_exact, engine_portfolio) =
        engine_selection(request.engine.as_deref());
    let base_opts = VerifyAutoOptions {
        must_edge_inference,
        gate_reset: request.gate_reset.unwrap_or(true),
        symbolic_engine: engine_symbolic,
        exact_symbolic: engine_exact,
        portfolio: engine_portfolio,
        ..Default::default()
    };

    let report = mutate_and_compare(&sources, &yopts, &base_opts, mutation).map_err(|e| {
        ApiError::BadRequest {
            message: format!("sv mutate: {}", e.message),
            details: None,
        }
    })?;

    Ok(Json(SvMutateResponse {
        mutation: Some(report.mutation),
        targets: None,
        properties: report
            .properties
            .into_iter()
            .map(|p| SvMutatePropertyFlip {
                name: p.name,
                baseline: p.baseline,
                mutant: p.mutant,
                flipped: p.flipped,
            })
            .collect(),
        flipped: report.flipped,
        unflipped: report.unflipped,
    }))
}

/// cegar-extraction Stage 2 (2026-06-22) — SV-direct CEGAR in one call.
///
/// Lifts SystemVerilog to a single flattened BTOR2 (sv2v + Yosys, the
/// same server-side pipeline `/context/import` already runs) and then
/// runs the *identical* predicate-abstraction refinement loop as
/// [`btor2_cegar_handler`], returning the same [`Btor2CegarResponse`].
/// Surface peer of the CLI `mununu sv cegar`; lets the extraction-tab SV
/// workflow run CEGAR end-to-end without a manual
/// `sv emit-btor2-per-module` step.
pub async fn sv_cegar_handler(
    Json(request): Json<SvCegarRequest>,
) -> ApiResult<Json<Btor2CegarResponse>> {
    blocking(move || sv_cegar_handler_impl(request)).await
}

fn sv_cegar_handler_impl(request: SvCegarRequest) -> ApiResult<Json<Btor2CegarResponse>> {
    use crate::adapter::yosys::{YosysOptions, sv_to_btor2};

    if request.predicates.is_empty() {
        return Err(ApiError::BadRequest {
            message: "at least one predicate is required to bootstrap the cube space".to_string(),
            details: None,
        });
    }

    // SV → single flattened BTOR2 (sv2v optional + Yosys). The per-module
    // split is for composition; CEGAR's predicate-cube lift wants one
    // transition system, so the flattened shape is the right input.
    let yopts = YosysOptions {
        top: request.top.clone(),
        additional_sources: request
            .additional_sources
            .iter()
            .map(|f| (f.name.clone(), f.content.clone()))
            .collect(),
        use_sv2v: request.use_sv2v,
        setundef_anyseq: request.setundef_anyseq,
        setundef_anyconst: request.setundef_anyconst,
        ..Default::default()
    };
    let btor2 = sv_to_btor2(&request.source, &yopts).map_err(|e| ApiError::BadRequest {
        message: format!("SV → BTOR2 (sv2v + Yosys): {}", e.message),
        details: None,
    })?;

    let params = CegarRunParams {
        btor2_content: &btor2,
        formula: &request.formula,
        predicates: &request.predicates,
        controllable_inputs: &request.controllable_inputs,
        predicate_source: request.predicate_source.as_deref(),
        max_iterations: request.max_iterations,
        must_edge_inference: request.must_edge_inference.as_deref(),
        may_edge_inference: request.may_edge_inference.as_deref(),
        config_values: &request.config_values,
        emit_ctxdsl: request.emit_ctxdsl,
        engine: request.engine.as_deref(),
    };
    Ok(Json(run_cegar_build_response(params)?))
}

/// XL.6a — `POST /api/v1/sv/extract-sva`. Runs the slang SVA front-end over the
/// SV source(s) and returns the translated mu-calculus property set (formulas +
/// recoverability companions + honestly-recorded unsupported assertions + the
/// `__past` shadows the formulas need). No model verification (that is
/// `/sv/verify-auto`). Surface peer of the CLI `mununu sv extract-sva`.
pub async fn sv_extract_sva_handler(
    Json(request): Json<SvExtractSvaRequest>,
) -> ApiResult<Json<SvExtractSvaResponse>> {
    blocking(move || sv_extract_sva_handler_impl(request)).await
}

fn sv_extract_sva_handler_impl(
    request: SvExtractSvaRequest,
) -> ApiResult<Json<SvExtractSvaResponse>> {
    use crate::adapter::slang::extract::extract_sva;
    use crate::adapter::slang::translate::SvaKind;

    fn kind_str(k: SvaKind) -> String {
        match k {
            SvaKind::Assert => "assert".to_string(),
            SvaKind::Assume => "assume".to_string(),
            SvaKind::Cover => "cover".to_string(),
        }
    }

    let mut sources: Vec<(String, String)> = vec![("top.sv".to_string(), request.source.clone())];
    for f in &request.additional_sources {
        sources.push((f.name.clone(), f.content.clone()));
    }

    let report = extract_sva(&sources).map_err(|e| ApiError::BadRequest {
        message: format!("SVA extraction (slang): {}", e.message),
        details: None,
    })?;

    let response = SvExtractSvaResponse {
        translated: report
            .translated
            .iter()
            .map(|t| TranslatedAssertionView {
                name: t.name.clone(),
                kind: kind_str(t.kind),
                formula: t.formula.clone(),
                recoverability_companion: t.recoverability_companion.clone(),
            })
            .collect(),
        unsupported: report
            .unsupported
            .iter()
            .map(|u| UnsupportedAssertionView {
                name: u.name.clone(),
                kind: u.kind.map(kind_str),
                reason: u.reason.clone(),
            })
            .collect(),
        required_shadows: report
            .required_shadows
            .iter()
            .map(|s| ShadowSignalView {
                base: s.base.clone(),
                width: s.width,
                depth: s.depth,
            })
            .collect(),
    };
    Ok(Json(response))
}

/// XL.6b — `POST /api/v1/sv/verify-auto`. Extract the design's SVA, lift, and
/// verify each property against the model with no sidecar (per-property
/// auto-seeded cube predicates → CEGAR). Surface peer of `mununu sv verify-auto`.
pub async fn sv_verify_auto_handler(
    Json(request): Json<SvVerifyAutoRequest>,
) -> ApiResult<Json<SvVerifyAutoResponse>> {
    blocking(move || sv_verify_auto_handler_impl(request)).await
}

fn sv_verify_auto_handler_impl(
    request: SvVerifyAutoRequest,
) -> ApiResult<Json<SvVerifyAutoResponse>> {
    use crate::adapter::btor2::kmts_lift::MustEdgeInference;
    use crate::adapter::slang::verify_auto::{VerifyAutoOptions, verify_auto};
    use crate::adapter::yosys::YosysOptions;

    let mut sources: Vec<(String, String)> = vec![("top.sv".to_string(), request.source.clone())];
    for f in &request.additional_sources {
        sources.push((f.name.clone(), f.content.clone()));
    }
    // `"NAME=VALUE"` / `"MODULE.NAME=VALUE"` module-parameter overrides. Unlike
    // `config_values` (which silently skips malformed entries), a malformed
    // `params` entry is a 400 (and yosys errors on one it cannot apply) — never a
    // silent drop.
    let params: Vec<(String, String)> = request
        .params
        .iter()
        .map(|e| {
            let (lhs, val) = e.split_once('=').ok_or_else(|| ApiError::BadRequest {
                message: format!("malformed params entry '{e}'"),
                details: Some("expected NAME=VALUE or MODULE.NAME=VALUE".into()),
            })?;
            let (lhs, val) = (lhs.trim(), val.trim());
            if lhs.is_empty() || val.is_empty() {
                return Err(ApiError::BadRequest {
                    message: format!("malformed params entry '{e}'"),
                    details: Some("NAME and VALUE must both be non-empty".into()),
                });
            }
            Ok((lhs.to_string(), val.to_string()))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let yopts = YosysOptions {
        top: request.top.clone(),
        additional_sources: request
            .additional_sources
            .iter()
            .map(|f| (f.name.clone(), f.content.clone()))
            .collect(),
        use_sv2v: request.use_sv2v,
        cutpoint_signals: request.cutpoint.clone(),
        frontend: if request.use_slang {
            crate::adapter::yosys::SvFrontend::Slang
        } else {
            crate::adapter::yosys::SvFrontend::Auto
        },
        params,
        ..Default::default()
    };
    let must_edge_inference = match request.must_edge_inference.as_deref() {
        Some("smt-per-target") => MustEdgeInference::SmtPerTarget,
        Some("smt-per-target-standard") => MustEdgeInference::SmtPerTargetStandard,
        Some("smt-hyper-must") => MustEdgeInference::SmtHyperMust,
        _ => MustEdgeInference::Off,
    };
    // H.J.b — parse `"signal=value"` config concretization entries. mununu#459: a
    // malformed entry is a 400 (mirrors `params` above), never a silent drop; a
    // value that is not a decimal u64 (e.g. hex `0x1`) is rejected by name. An
    // unknown / non-input signal is caught downstream in `verify_auto` against the
    // model's real primary inputs.
    let config_values: std::collections::HashMap<String, u64> = request
        .config_values
        .iter()
        .map(|e| {
            let (name, val) = e.split_once('=').ok_or_else(|| ApiError::BadRequest {
                message: format!("malformed config_values entry '{e}'"),
                details: Some("expected \"SIGNAL=VALUE\"".into()),
            })?;
            let (name, val) = (name.trim(), val.trim());
            if name.is_empty() {
                return Err(ApiError::BadRequest {
                    message: format!("malformed config_values entry '{e}'"),
                    details: Some("SIGNAL must be non-empty".into()),
                });
            }
            let value = val.parse::<u64>().map_err(|_| ApiError::BadRequest {
                message: format!("malformed config_values entry '{e}'"),
                details: Some(format!(
                    "VALUE must be a decimal u64 (got '{val}'); hex/0x, signed, \
                     and non-numeric values are not accepted"
                )),
            })?;
            Ok((name.to_string(), value))
        })
        .collect::<Result<std::collections::HashMap<_, _>, ApiError>>()?;
    // H.H — parse `"signal<=value"` (or `"signal=value"`) counter-bound entries;
    // both spellings mean the inclusive upper bound `signal <= value`.
    let counter_bounds: std::collections::HashMap<String, u64> = request
        .counter_bounds
        .iter()
        .filter_map(|e| {
            let (name, val) = e.split_once("<=").or_else(|| e.split_once('='))?;
            Some((name.trim().to_string(), val.trim().parse::<u64>().ok()?))
        })
        .collect();
    let (engine_symbolic, engine_exact, engine_portfolio) =
        crate::adapter::slang::verify_auto::engine_selection(request.engine.as_deref());
    let opts = VerifyAutoOptions {
        max_iterations: request.max_iterations.unwrap_or(16),
        must_edge_inference,
        gate_reset: request.gate_reset.unwrap_or(true),
        auto_stub_flops: request.auto_stub_flops.unwrap_or(true),
        config_values,
        counter_bounds,
        predicate_hints: request.predicate.clone(),
        // Engine selection (`"explicit"` | `"symbolic"` | `"exact-symbolic"` |
        // `"portfolio-sequential"` | `"portfolio-parallel"`). Unspecified ⇒ the
        // default `portfolio-sequential`. `engine_selection` is the single place the
        // string→options mapping + default live (mirrors the CLI value-enum default).
        symbolic_engine: engine_symbolic,
        exact_symbolic: engine_exact,
        portfolio: engine_portfolio,
        rescue_bottom_safety: request.rescue_bottom_safety.unwrap_or(true),
        rescue_bottom_liveness: request.rescue_bottom_liveness.unwrap_or(true),
        rescue_bottom_recoverability: request.rescue_bottom_recoverability.unwrap_or(true),
        // `verify-auto` verifies the design as-lifted; mutation is the `sv mutate` verb's job.
        mutation: None,
        // mununu#476 item 4 — per-request antecedent shadow-synth opt-out.
        // `no_antecedent_shadow: true` reverts to the Phase A refusal; default
        // (unspecified / false) keeps the shipped shadow-synth behaviour.
        antecedent_shadow: !request.no_antecedent_shadow.unwrap_or(false),
    };

    let report = verify_auto(&sources, &yopts, &opts).map_err(|e| ApiError::BadRequest {
        message: format!("verify-auto: {}", e.message),
        details: None,
    })?;

    // mununu#536 — ONE conversion, shared with the CLI's `--json`. This used to be ~90 lines
    // of hand-written field mapping that had already drifted from the CLI's independent
    // `serde_json::json!` literal; see the `From` impl's doc comment for why there is now
    // exactly one.
    Ok(Json(SvVerifyAutoResponse::from(&report)))
}

/// Shared parameters for the CEGAR run/report logic, sourced identically
/// from [`Btor2CegarRequest`] (raw BTOR2) and [`SvCegarRequest`] (SV
/// lifted to BTOR2 first).
struct CegarRunParams<'a> {
    btor2_content: &'a str,
    formula: &'a str,
    predicates: &'a [PredicateSpecRequest],
    controllable_inputs: &'a [String],
    predicate_source: Option<&'a str>,
    max_iterations: Option<usize>,
    must_edge_inference: Option<&'a str>,
    may_edge_inference: Option<&'a str>,
    config_values: &'a [String],
    emit_ctxdsl: bool,
    engine: Option<&'a str>,
}

/// R-F5.4.2b — the `engine=symbolic` path: evaluate the property over the
/// predicate-cube abstraction via the R-F5 symbolic BDD relation (no
/// per-cube-pair SMT), single-shot at the given predicate set. Returns the same
/// [`Btor2CegarResponse`] shape as the explicit path (empty `iterations`,
/// `terminated_with = "symbolic-single-shot"`, the `{T,F,⊥}` verdict summary).
fn run_symbolic_cegar_response(
    params: &CegarRunParams<'_>,
    predicates: &[crate::adapter::btor2::kmts_lift::PredicateSpec],
    formula: &crate::mu_calculus::Formula,
) -> Result<Btor2CegarResponse, ApiError> {
    use crate::adapter::AdapterOptions;
    use crate::adapter::btor2::cegar::config_values_to_sidecar_json;
    use crate::adapter::btor2::symbolic_bitblast::MustSemantics;
    use crate::adapter::btor2::symbolic_engine::{SymbolicCegarTermination, symbolic_cegar_refine};

    // Build the same config-values synthetic sidecar the explicit path uses, so
    // any non-derived `compound_predicates` become cube dimensions (R-F5.5a).
    // The cegar request has no explicit sidecar field, so today this is
    // equality-only unless config-values embed a compound — parity with the
    // explicit API path.
    let sidecar_json = config_values_to_sidecar_json(params.config_values).map_err(|message| {
        ApiError::BadRequest {
            message,
            details: None,
        }
    })?;
    let options = AdapterOptions {
        sidecar_json,
        ..AdapterOptions::default()
    };
    // R-F5.5b — the symbolic CEGAR loop (WP refinement on ⊥; no per-cube-pair SMT).
    let max_iterations = params.max_iterations.unwrap_or(16);
    let result = symbolic_cegar_refine(
        params.btor2_content,
        predicates,
        &options,
        formula,
        MustSemantics::ForallExists,
        max_iterations,
    )
    .map_err(|e| ApiError::BadRequest {
        message: format!("symbolic cube engine: {}", e.message),
        details: None,
    })?;

    let iterations = result
        .iterations
        .iter()
        .map(|it| CegarIterationView {
            iteration: it.iteration,
            predicate_count: it.predicate_count,
            had_failure_subgame: it.bottom > 0,
            // The symbolic MVP does not track which predicate closed each ⊥.
            predicates_added: Vec::new(),
            game_position_evaluations: 0,
            verdict: CegarVerdictSummary {
                true_cells: it.definite_true,
                false_cells: it.definite_false,
                unknown_cells: it.bottom,
            },
        })
        .collect();
    let terminated_with = match result.terminated_with {
        SymbolicCegarTermination::Converged => "converged",
        SymbolicCegarTermination::BoundedIterationsReached => "bounded-iterations-reached",
        SymbolicCegarTermination::PredicateSourceExhausted => "predicate-source-exhausted",
    }
    .to_string();
    let v = &result.final_verdicts;

    Ok(Btor2CegarResponse {
        success: true,
        iterations,
        final_predicates: result
            .final_predicates
            .iter()
            .map(|p| PredicateView {
                name: p.name.clone(),
                register: p.register.clone(),
                value: p.value,
            })
            .collect(),
        terminated_with,
        verdict: CegarVerdictSummary {
            true_cells: v.definite_true,
            false_cells: v.definite_false,
            unknown_cells: v.bottom,
        },
        lazy_lift_pending: false,
        approximant_reuse_enabled: false,
        warnings: vec![
            "engine=symbolic (R-F5): BDD relation + WP refinement, no per-cube-pair SMT; \
             simple + non-derived-compound predicates + bare []/<> fragment only"
                .to_string(),
        ],
        violating_cells: Vec::new(),
        undecided_cells: Vec::new(),
        counterexample: None,
        refinement_candidates: Vec::new(),
        ctxdsl: None,
    })
}

/// Run the predicate-abstraction refinement loop over a BTOR2 design and
/// build the JSON response. Shared by `btor2_cegar_handler` (BTOR2-direct)
/// and `sv_cegar_handler` (SV lifted to BTOR2 first) so the two surfaces
/// stay byte-for-byte in lockstep on the CEGAR semantics + report shape.
fn run_cegar_build_response(params: CegarRunParams<'_>) -> Result<Btor2CegarResponse, ApiError> {
    use crate::adapter::AdapterOptions;
    use crate::adapter::btor2::cegar::{
        CegarOptions, CegarTermination, LiftStrategy, PredicateSource, cegar_refine_loop,
        config_values_to_sidecar_json,
    };
    use crate::adapter::btor2::kmts_lift::{MayEdgeInference, MustEdgeInference, PredicateSpec};
    use crate::mu_calculus::trit::{Trit, TritSet};
    use crate::mu_calculus::{Environment, parser as mu_parser};

    let predicates: Vec<PredicateSpec> = params
        .predicates
        .iter()
        .map(|p| PredicateSpec {
            name: p.name.clone(),
            register: p.register.clone(),
            value: p.value,
        })
        .collect();

    let formula = mu_parser::parse(params.formula).map_err(|e| ApiError::BadRequest {
        message: format!("formula parse error: {e:?}"),
        details: None,
    })?;

    // R-F5.4.2b — the symbolic engine short-circuits the explicit lift + CEGAR
    // loop: build the may/must relation as BDDs directly (no per-cube-pair SMT)
    // and evaluate. Single-shot at the given predicate set; simple equality
    // predicates + the bare `[]`/`<>` fragment only.
    if matches!(params.engine, Some(e) if e.eq_ignore_ascii_case("symbolic")) {
        return run_symbolic_cegar_response(&params, &predicates, &formula);
    }

    // Environment is sized to the cube space (2^|predicates|), matching the
    // CLI `btor2 cegar` bootstrap.
    let env = Environment::new(1usize << predicates.len());

    let predicate_source = match params.predicate_source {
        Some("craig") => PredicateSource::CraigInterpolation,
        _ => PredicateSource::WeakestPrecondition,
    };
    let must_edge_inference = match params.must_edge_inference {
        Some("smt-per-target") => MustEdgeInference::SmtPerTarget,
        Some("smt-per-target-standard") => MustEdgeInference::SmtPerTargetStandard,
        Some("smt-hyper-must") => MustEdgeInference::SmtHyperMust,
        _ => MustEdgeInference::Off,
    };
    // M.6 parity (2026-06-20) — the may-edge inference policy is now an API
    // field (was CLI-only). `smt-all-pairs` selects the sound all-pairs
    // may-relation, matching `--may-edge-inference`; default is Off.
    let may_edge_inference = match params.may_edge_inference {
        Some("smt-all-pairs") => MayEdgeInference::SmtAllPairs,
        _ => MayEdgeInference::Off,
    };

    let cegar_opts = CegarOptions {
        max_iterations: params.max_iterations.unwrap_or(16),
        predicate_source,
        max_cube_count: 1024,
        capture_approximants: false,
        enable_approximant_reuse: false,
        smart_uf_cap: true,
        lift_strategy: LiftStrategy::Eager,
        must_edge_inference,
        may_edge_inference,
        // CTXDSL Phase 2 — capture the final cube model only when the
        // request opts in via `emit_ctxdsl`.
        emit_ctxdsl: params.emit_ctxdsl,
    };

    // M.6 parity (2026-06-20) — config-values symbolic init is now an API
    // field (was CLI-only). The shared `config_values_to_sidecar_json` helper
    // gives the CLI and the API one parse for the `REG=v1,v2,...` format; the
    // synthetic sidecar threads through to the predicate-cube lift's
    // `config_values` (R-S8 init expansion — e.g. the M.4 boot_fsm_ns hazard).
    let sidecar_json = config_values_to_sidecar_json(params.config_values).map_err(|message| {
        ApiError::BadRequest {
            message,
            details: None,
        }
    })?;
    let adapter_options = AdapterOptions {
        controllable_inputs: params.controllable_inputs.to_vec(),
        sidecar_json,
        ..Default::default()
    };

    let trace = cegar_refine_loop(
        &formula,
        params.btor2_content,
        predicates,
        &env,
        &adapter_options,
        &cegar_opts,
    )
    .map_err(|e| ApiError::BadRequest {
        message: format!("CEGAR refine loop: {}", e.message),
        details: None,
    })?;

    let summarize = |v: &TritSet| -> CegarVerdictSummary {
        let mut s = CegarVerdictSummary {
            true_cells: 0,
            false_cells: 0,
            unknown_cells: 0,
        };
        for i in 0..v.len() {
            match v.verdict_at(i) {
                Trit::True => s.true_cells += 1,
                Trit::False => s.false_cells += 1,
                Trit::Unknown => s.unknown_cells += 1,
            }
        }
        s
    };
    let pred_view = |p: &PredicateSpec| PredicateView {
        name: p.name.clone(),
        register: p.register.clone(),
        value: p.value,
    };

    // Track I.1 — viewer-shape a witness cell (cube index + predicate valuation).
    // Cap matches the CLI so a large cube does not flood the response; the
    // `verdict.{false,unknown}_cells` counts carry the full totals.
    const WITNESS_CELL_CAP: usize = 8;
    let cell_view =
        |c: &crate::adapter::btor2::cegar::WitnessCell| crate::api::models::WitnessCellView {
            cube_index: c.cube_index,
            valuation: c.valuation.iter().cloned().collect(),
        };

    // CTXDSL Phase 2 (2026-06-22) — opt-in model + formula CTXDSL. When the
    // request set `emit_ctxdsl`, the loop captured the final refined cube
    // `Clts` into `trace.final_clts`; serialize it together with the checked
    // formula (the original request string). Absent ⇒ `None` ⇒ the `ctxdsl`
    // response field is omitted.
    let ctxdsl = if params.emit_ctxdsl {
        match &trace.final_clts {
            Some(clts) => Some(
                crate::adapter::clts_to_ir::clts_to_ctxdsl_with_formula(
                    clts,
                    "lifted_kmts",
                    "cegar_model",
                    "checked_property",
                    params.formula,
                )
                .map_err(|e| ApiError::BadRequest {
                    message: format!("emit ctxdsl: {}", e.message),
                    details: None,
                })?,
            ),
            None => None,
        }
    } else {
        None
    };

    let iterations = trace
        .iterations
        .iter()
        .map(|it| CegarIterationView {
            iteration: it.iteration,
            predicate_count: it.predicates_at_start.len(),
            had_failure_subgame: it.failure_subgame.is_some(),
            predicates_added: it.predicates_added.iter().map(&pred_view).collect(),
            game_position_evaluations: it.game_position_evaluations,
            verdict: summarize(&it.verdict),
        })
        .collect();

    Ok(Btor2CegarResponse {
        success: true,
        iterations,
        final_predicates: trace.final_predicates.iter().map(&pred_view).collect(),
        terminated_with: match trace.terminated_with {
            CegarTermination::Converged => "converged",
            CegarTermination::BoundedIterationsReached => "bounded-iterations-reached",
            CegarTermination::PredicateSourceExhausted => "predicate-source-exhausted",
            // mununu#504 — a NEW wire value. Distinct from "bounded-iterations-reached": that
            // means the refinement cap was reached, this means the wall clock ran out. They
            // suggest different remedies (more rounds vs more time), so a consumer must be able
            // to tell them apart.
            CegarTermination::BudgetExpired => "budget-expired",
        }
        .to_string(),
        verdict: summarize(&trace.final_verdict),
        lazy_lift_pending: trace.lazy_lift_pending,
        approximant_reuse_enabled: trace.approximant_reuse_enabled,
        warnings: trace.warnings.iter().map(|w| w.message.clone()).collect(),
        // Track I.1 — surface which cube valuations falsify / can't be decided.
        violating_cells: trace
            .violating_cells(WITNESS_CELL_CAP)
            .iter()
            .map(&cell_view)
            .collect(),
        undecided_cells: trace
            .undecided_cells(WITNESS_CELL_CAP)
            .iter()
            .map(&cell_view)
            .collect(),
        // Track I.1 (trace slice) — the reachability countertrace for a
        // violated verdict (None when not violated at the initial cell).
        counterexample: trace.counterexample.as_ref().map(|ct| {
            crate::api::models::CounterTraceView {
                steps: ct.steps.iter().map(&cell_view).collect(),
                ends_in_trap: ct.ends_in_trap,
            }
        }),
        // Track I.1 (undecided-explanation) — registers the failure subgame
        // flagged as load-bearing for the remaining ⊥ cells (empty ⇒ omitted).
        refinement_candidates: trace.init_refinement_candidates.clone(),
        ctxdsl,
    })
}

/// Generate graph data for visualization
pub async fn context_graphs_handler(
    Json(request): Json<ContextGraphsRequest>,
) -> ApiResult<Json<ContextGraphsResponse>> {
    blocking(move || context_graphs_handler_impl(request)).await
}

fn context_graphs_handler_impl(
    request: ContextGraphsRequest,
) -> ApiResult<Json<ContextGraphsResponse>> {
    let handler_start = Instant::now();

    // Cache parse + realize.
    let sidecar_strs: Vec<&str> = request
        .sidecars
        .iter()
        .map(|s| s.content.as_str())
        .collect();
    let (entry, cache_hit) =
        crate::api::cache::get_or_realize(&request.context.content, &sidecar_strs).map_err(
            |e| ApiError::BadRequest {
                message: format!("Failed to parse/realize context: {}", e),
                details: Some(e.clone()),
            },
        )?;
    let context_doc = entry.context_doc.as_ref();
    let sidecar_docs = entry.sidecar_docs.as_ref();
    let realized = entry.realized.as_ref();
    info!(
        realize_ms = handler_start.elapsed().as_millis() as u64,
        cache_hit, "graphs: parse+realize complete"
    );

    // Generate graphs
    let (mut graphs, context_summary) = generate_graphs(
        context_doc,
        sidecar_docs,
        realized,
        request.automaton.as_deref(),
        &request.graph_types,
    )
    .map_err(|e| ApiError::Internal {
        message: format!("Failed to generate graphs: {}", e),
        source: None,
    })?;

    // Synthesize declared controllers and add their graphs
    if request.include_controllers {
        let eval_options = EvaluationOptions::default();
        for rc in realized.controllers.values() {
            let Some(rf) = realized.formulas.get(&rc.formula) else {
                continue;
            };
            if realized.context.clts(&rc.source).is_none() {
                continue;
            }
            let env = realized.environment_for(&rc.source);
            let Ok(syn) = realized.context.synthesise_controller_with_options(
                &rc.source,
                &rf.formula,
                &env,
                ControllerSynthesisOptions {
                    evaluation: Some(&eval_options),
                    diagnostics: None,
                    minimize: request
                        .minimize_controllers
                        .unwrap_or(rc.options.minimize()),
                    extract_strategy: false,
                    mode: crate::context::ControllerMode::default(),
                },
            ) else {
                continue;
            };
            if !syn.realizable {
                continue;
            }
            let controller_name = format!("{}_controller", rc.name);
            let Some(source_clts) = realized.context.clts(&rc.source) else {
                continue;
            };
            if let Ok(elements) = crate::api::graph::controller_to_graph_elements(
                &syn.controller,
                source_clts,
                &controller_name,
            ) {
                let metadata =
                    crate::api::graph::calculate_graph_metadata_pub(&elements, &controller_name);
                graphs.push(GraphData {
                    automaton: controller_name,
                    graph_type: GraphTypeResponse::Controller,
                    elements,
                    metadata,
                });
            }
        }
    }

    Ok(Json(ContextGraphsResponse {
        success: true,
        context: context_summary,
        graphs,
    }))
}

/// Verify context by evaluating μ-calculus formulas over automata
pub async fn context_verify_handler(
    Json(request): Json<ContextVerifyRequest>,
) -> ApiResult<Json<ContextVerifyResponse>> {
    blocking(move || context_verify_handler_impl(request)).await
}

fn context_verify_handler_impl(
    request: ContextVerifyRequest,
) -> ApiResult<Json<ContextVerifyResponse>> {
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
            satisfied,
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

    let all_satisfied = results.iter().all(|r| r.satisfied);

    Ok(Json(ContextVerifyResponse {
        success: true,
        all_satisfied,
        results,
    }))
}

/// Resolve formula targets to a list of automaton names
fn resolve_targets(
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
fn compute_counterstrategy_result(
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

/// Convert controller diagnostics to API format
fn convert_diagnostics(
    diagnostics: &crate::context::ControllerDiagnostics,
) -> SynthesisDiagnostics {
    SynthesisDiagnostics {
        messages: diagnostics.messages.clone(),
        violating_initials: diagnostics.violating_initials.clone(),
        counterexample_trace: diagnostics.counterexample_trace.clone(),
        counterstrategy_traces: diagnostics.counterstrategy_traces.clone(),
        deadlock_traces: diagnostics.deadlock_traces.clone(),
        minimization: diagnostics
            .minimization
            .as_ref()
            .map(|m| MinimizationReport {
                removed_states: m.removed_states,
                removed_transitions: m.removed_transitions,
                merged_states: m.merged_states.clone(),
            }),
        proof_obligations: diagnostics
            .proof_obligations
            .iter()
            .map(|po| ProofObligation {
                state: po.state.clone(),
                detail: po.detail.clone(),
            })
            .collect(),
        lasso_traces: diagnostics
            .lasso_traces
            .iter()
            .map(|lt| LassoTraceApi {
                prefix: lt.prefix.clone(),
                cycle: lt.cycle.clone(),
                prefix_labels: lt.prefix_labels.clone(),
                cycle_labels: lt.cycle_labels.clone(),
            })
            .collect(),
    }
}

/// Serialize controller CLTS to ctxdsl format
fn serialize_controller_to_ctxdsl(
    controller: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    automaton: &str,
    formula: &str,
    raw_formula: &str,
) -> ApiResult<String> {
    use std::fmt::Write;

    let mut output = String::new();

    // Collect all labels
    let mut ordered_labels: BTreeMap<usize, LabelId<DefaultLabelIdx>> = BTreeMap::new();
    for state in controller.states() {
        for transition in controller.outgoing(state) {
            for label_id in transition.labels() {
                ordered_labels.entry(label_id.index()).or_insert(*label_id);
            }
        }
    }

    // Generate label names
    let mut label_names = HashMap::new();
    let mut label_entries = Vec::new();
    let mut seen_label_idents = HashSet::new();
    for (index, label_id) in ordered_labels {
        let mut payload = controller
            .label_payload(label_id)
            .map(|values| values.to_vec())
            .unwrap_or_default();
        payload.sort();
        let base = if payload.is_empty() {
            format!("label_{index}")
        } else {
            payload.join("_")
        };
        let mut ident = sanitize_identifier(&base);
        if ident.is_empty() {
            ident = format!("label_{index}");
        }
        let original_ident = ident.clone();
        if !seen_label_idents.insert(ident.clone()) {
            let mut counter = 1usize;
            loop {
                let candidate = format!("{original_ident}_{counter}");
                if seen_label_idents.insert(candidate.clone()) {
                    ident = candidate;
                    break;
                }
                counter += 1;
            }
        }
        label_names.insert(label_id, ident.clone());
        label_entries.push((label_id, ident, payload));
    }

    // Generate state names
    let mut state_idents = Vec::with_capacity(controller.state_count());
    let mut raw_state_names = Vec::with_capacity(controller.state_count());
    let mut seen_state_idents = HashSet::new();
    for state in controller.states() {
        let idx = state.index();
        if state_idents.len() <= idx {
            state_idents.resize(idx + 1, String::new());
            raw_state_names.resize(idx + 1, String::new());
        }
        let raw = controller
            .state_name(state)
            .map(|name| name.to_owned())
            .unwrap_or_else(|| format!("state_{idx}"));
        let mut ident = sanitize_identifier(&raw);
        if ident.is_empty() {
            ident = format!("state_{idx}");
        }
        let original_ident = ident.clone();
        if !seen_state_idents.insert(ident.clone()) {
            let mut counter = 1usize;
            loop {
                let candidate = format!("{original_ident}_{counter}");
                if seen_state_idents.insert(candidate.clone()) {
                    ident = candidate;
                    break;
                }
                counter += 1;
            }
        }
        state_idents[idx] = ident;
        raw_state_names[idx] = raw;
    }

    // Generate context and automaton identifiers
    let context_ident = sanitize_identifier(&format!("{}_{}_controller", automaton, formula));
    let automaton_ident = format!("{context_ident}_automaton");

    // Write header comment
    writeln!(
        output,
        "// Synthesised controller derived from automaton '{}' and formula '{}'",
        automaton, formula
    )
    .map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;

    // Write context declaration
    writeln!(output, "context {context_ident} {{").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;

    // Write alphabet
    if !label_entries.is_empty() {
        writeln!(output, "    alphabet {{").map_err(|e| ApiError::Internal {
            message: format!("Failed to write controller DSL: {}", e),
            source: None,
        })?;
        for (_, ident, payload) in &label_entries {
            if payload.is_empty() {
                writeln!(output, "        label {ident};").map_err(|e| ApiError::Internal {
                    message: format!("Failed to write controller DSL: {}", e),
                    source: None,
                })?;
            } else {
                writeln!(
                    output,
                    "        label {ident}; // original symbols: {}",
                    payload.join(", ")
                )
                .map_err(|e| ApiError::Internal {
                    message: format!("Failed to write controller DSL: {}", e),
                    source: None,
                })?;
            }
        }
        writeln!(output, "    }}").map_err(|e| ApiError::Internal {
            message: format!("Failed to write controller DSL: {}", e),
            source: None,
        })?;
    }

    // Write automata section
    writeln!(output, "    automata {{").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "        automaton {automaton_ident} {{").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;

    // Write states
    writeln!(output, "            states {{").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    for state in controller.states() {
        let idx = state.index();
        let name = &state_idents[idx];
        let raw = &raw_state_names[idx];
        let mut line = format!("                state {name}");
        if controller.initial_states().contains(&state) {
            line.push_str(" initial");
        }
        line.push(';');
        if raw != name {
            line.push_str(" // original: ");
            line.push_str(raw);
        }
        writeln!(output, "{line}").map_err(|e| ApiError::Internal {
            message: format!("Failed to write controller DSL: {}", e),
            source: None,
        })?;
    }
    writeln!(output, "            }}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;

    // Write transitions
    writeln!(output, "            transitions {{").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    for state in controller.states() {
        let source_name = &state_idents[state.index()];
        for transition in controller.outgoing(state) {
            let target_name = &state_idents[transition.target().index()];
            let mut clauses = Vec::new();
            for label_id in transition.labels() {
                if let Some(name) = label_names.get(label_id) {
                    clauses.push(format!("label {name}"));
                }
            }
            let labels_clause = if clauses.is_empty() {
                "epsilon".to_owned()
            } else {
                clauses.join(", ")
            };
            let mut line = format!(
                "                transition {source} -> {target} on {labels};",
                source = source_name,
                target = target_name,
                labels = labels_clause
            );
            if transition.is_controllable(controller) {
                line.push_str(" // controllable");
            } else {
                line.push_str(" // uncontrollable");
            }
            writeln!(output, "{line}").map_err(|e| ApiError::Internal {
                message: format!("Failed to write controller DSL: {}", e),
                source: None,
            })?;
        }
    }
    writeln!(output, "            }}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "        }}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "    }}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;

    // Write mu_formulas section
    writeln!(output, "    mu_formulas {{").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(
        output,
        "        formula {} {{",
        sanitize_identifier(formula)
    )
    .map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "            over {automaton_ident};").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "            body = {raw_formula};").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "        }}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "    }}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;
    writeln!(output, "}}").map_err(|e| ApiError::Internal {
        message: format!("Failed to write controller DSL: {}", e),
        source: None,
    })?;

    Ok(output)
}

// ============================================================================
// Extraction Endpoints
// ============================================================================

/// List available domain profiles for extraction.
pub async fn extraction_domains_handler()
-> ApiResult<Json<super::models::ExtractionDomainsResponse>> {
    use crate::adapter::extraction::ast_extract::domain;

    let profiles = domain::available_profiles()
        .into_iter()
        .filter_map(|name| {
            let profile = domain::get_profile(name)?;
            Some(super::models::DomainProfileInfo {
                name: name.to_string(),
                language: profile.language.to_string(),
                description: profile.description.to_string(),
            })
        })
        .collect();

    Ok(Json(super::models::ExtractionDomainsResponse { profiles }))
}

/// Phase B — scan source for concurrency idioms and return findings
/// the caller can use to seed a `composition.instances[]` block. Pure
/// pass-through to `concurrency_detect::detect_concurrency`.
#[cfg(feature = "ast-extract")]
pub async fn extraction_propose_composition_handler(
    Json(request): Json<super::models::ProposeCompositionRequest>,
) -> ApiResult<Json<super::models::ProposeCompositionResponse>> {
    blocking(move || extraction_propose_composition_handler_impl(request)).await
}

#[cfg(feature = "ast-extract")]
fn extraction_propose_composition_handler_impl(
    request: super::models::ProposeCompositionRequest,
) -> ApiResult<Json<super::models::ProposeCompositionResponse>> {
    use crate::adapter::extraction::ast_extract::{concurrency_detect, parser};

    let lang_name = request
        .language
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest {
            message: "missing required field `language`".to_string(),
            details: Some("supported values: typescript, python, rust".to_string()),
        })?;
    let lang =
        parser::SourceLanguage::from_name(lang_name).ok_or_else(|| ApiError::BadRequest {
            message: format!("unknown language: {lang_name}"),
            details: Some("supported values: typescript, python, rust".to_string()),
        })?;

    let parsed = parser::parse_source(&request.source, lang).map_err(|e| ApiError::BadRequest {
        message: format!("parse error: {e}"),
        details: None,
    })?;

    let findings = concurrency_detect::detect_concurrency(&parsed);
    Ok(Json(super::models::ProposeCompositionResponse { findings }))
}

/// Stub when ast-extract feature is not enabled. Mirrors the pattern used
/// for `extraction_extract_handler` so route registration in `server.rs`
/// can stay unconditional.
#[cfg(not(feature = "ast-extract"))]
pub async fn extraction_propose_composition_handler(
    Json(_request): Json<super::models::ProposeCompositionRequest>,
) -> ApiResult<Json<super::models::ProposeCompositionResponse>> {
    Err(ApiError::BadRequest {
        message: "AST extraction not available. Build with --features ast-extract".to_string(),
        details: None,
    })
}

/// List the supported composition modes consumed by `composition.type`
/// in the extract config / espec. Static — derived from the
/// `CompositionSemantics` enum's variants and their soundness notes.
pub async fn extraction_composition_modes_handler()
-> ApiResult<Json<super::models::CompositionModesResponse>> {
    let modes = vec![
        super::models::CompositionModeInfo {
            name: "synchronous",
            description: "Shared-alphabet labels fire jointly across instances; \
                          independent labels collapse into a single joint step.",
        },
        super::models::CompositionModeInfo {
            name: "asynchronous",
            description: "Shared-alphabet labels fire jointly; independent labels \
                          interleave in either order without fairness constraints. \
                          Sound for safety; unsound for liveness without explicit \
                          fairness assumptions.",
        },
    ];
    Ok(Json(super::models::CompositionModesResponse { modes }))
}

/// Extract a model from source code using the AST-based extraction pipeline.
///
/// Requires the `ast-extract` feature flag.
#[cfg(feature = "ast-extract")]
pub async fn extraction_extract_handler(
    Json(request): Json<super::models::ExtractionExtractRequest>,
) -> ApiResult<Json<super::models::ExtractionExtractResponse>> {
    blocking(move || extraction_extract_handler_impl(request)).await
}

#[cfg(feature = "ast-extract")]
fn extraction_extract_handler_impl(
    request: super::models::ExtractionExtractRequest,
) -> ApiResult<Json<super::models::ExtractionExtractResponse>> {
    let language = request.language.as_deref().unwrap_or("typescript");

    let spec = crate::adapter::extraction::ast_extract::extract_from_source(
        &request.config,
        &request.source,
        language,
    )
    .map_err(|e| ApiError::BadRequest {
        message: format!("Extraction failed: {e}"),
        details: None,
    })?;

    let automata: Vec<super::models::ExtractionAutomatonInfo> = spec
        .model_config
        .automata
        .iter()
        .map(|a| super::models::ExtractionAutomatonInfo {
            id: a.id.clone(),
            state_count: a.states.len(),
            transition_count: a.transitions.len(),
        })
        .collect();

    let espec_json = serde_json::to_string_pretty(&spec).map_err(|e| ApiError::Internal {
        message: format!("Failed to serialize extraction result: {e}"),
        source: None,
    })?;

    Ok(Json(super::models::ExtractionExtractResponse {
        success: true,
        espec: espec_json,
        warnings: vec![],
        automata,
    }))
}

/// Stub handler when ast-extract feature is not enabled.
#[cfg(not(feature = "ast-extract"))]
pub async fn extraction_extract_handler(
    Json(_request): Json<super::models::ExtractionExtractRequest>,
) -> ApiResult<Json<super::models::ExtractionExtractResponse>> {
    Err(ApiError::BadRequest {
        message: "AST extraction not available. Build with --features ast-extract".to_string(),
        details: None,
    })
}

/// Validate an extraction spec against source code.
pub async fn extraction_validate_handler(
    Json(request): Json<ExtractionValidateRequest>,
) -> ApiResult<Json<ExtractionValidateResponse>> {
    blocking(move || extraction_validate_handler_impl(request)).await
}

fn extraction_validate_handler_impl(
    request: ExtractionValidateRequest,
) -> ApiResult<Json<ExtractionValidateResponse>> {
    use crate::adapter::extraction::validate;

    let report =
        validate::validate_spec_content(&request.spec, &request.source, request.drift_window)
            .map_err(|e| ApiError::BadRequest {
                message: format!("Validation failed: {e}"),
                details: None,
            })?;

    let anchors: Vec<AnchorResultApi> = report
        .anchors
        .iter()
        .map(|a| match a {
            validate::AnchorResult::Exact {
                spec_id,
                section,
                line,
                ..
            } => AnchorResultApi {
                id: spec_id.clone(),
                section: section.clone(),
                status: "exact".to_string(),
                line: Some(*line),
                found_line: Some(*line),
                message: None,
            },
            validate::AnchorResult::Drifted {
                spec_id,
                section,
                expected_line,
                found_line,
                ..
            } => AnchorResultApi {
                id: spec_id.clone(),
                section: section.clone(),
                status: "drifted".to_string(),
                line: Some(*expected_line),
                found_line: Some(*found_line),
                message: Some(format!(
                    "Drifted from line {} to {}",
                    expected_line, found_line
                )),
            },
            validate::AnchorResult::Mismatch {
                spec_id,
                section,
                expected_line,
                expected_pattern,
                actual_at_line,
            } => AnchorResultApi {
                id: spec_id.clone(),
                section: section.clone(),
                status: "mismatch".to_string(),
                line: Some(*expected_line),
                found_line: None,
                message: Some(format!(
                    "Expected '{}', found '{}'",
                    expected_pattern, actual_at_line
                )),
            },
            validate::AnchorResult::Error {
                spec_id,
                section,
                message,
            } => AnchorResultApi {
                id: spec_id.clone(),
                section: section.clone(),
                status: "error".to_string(),
                line: None,
                found_line: None,
                message: Some(message.clone()),
            },
        })
        .collect();

    let uncovered: Vec<UncoveredAccessApi> = report
        .uncovered
        .iter()
        .map(|u| UncoveredAccessApi {
            line: u.line,
            field: u.field.clone(),
            content: u.content.clone(),
        })
        .collect();

    Ok(Json(ExtractionValidateResponse {
        success: true,
        summary: ValidationSummaryApi {
            total: report.summary.total,
            exact: report.summary.exact,
            drifted: report.summary.drifted,
            mismatch: report.summary.mismatch,
            error: report.summary.error,
            uncovered_accesses: report.summary.uncovered_accesses,
        },
        anchors,
        uncovered,
        commit_match: report.commit_match,
    }))
}

/// List predicate names per automaton in a context.
pub async fn context_predicates_handler(
    Json(request): Json<ContextPredicatesRequest>,
) -> ApiResult<Json<ContextPredicatesResponse>> {
    blocking(move || context_predicates_handler_impl(request)).await
}

fn context_predicates_handler_impl(
    request: ContextPredicatesRequest,
) -> ApiResult<Json<ContextPredicatesResponse>> {
    let instant = Instant::now();

    // Cache parse + realize.
    let sidecar_strs: Vec<&str> = request
        .sidecars
        .iter()
        .map(|s| s.content.as_str())
        .collect();
    let (entry, cache_hit) =
        crate::api::cache::get_or_realize(&request.context.content, &sidecar_strs).map_err(
            |e| ApiError::BadRequest {
                message: format!("Context parse/realize error: {e}"),
                details: None,
            },
        )?;
    let realized = entry.realized.as_ref();

    info!(
        elapsed_ms = instant.elapsed().as_millis() as u64,
        cache_hit, "predicates: realized"
    );

    let mut predicates: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for aut_name in realized.context.clts_names() {
        if let Some(ref filter) = request.automaton
            && &aut_name != filter
        {
            continue;
        }
        let preds: Vec<String> = realized
            .predicate_names(&aut_name)
            .map(|set| {
                let mut v: Vec<String> = set.iter().map(|s| s.to_string()).collect();
                v.sort();
                v
            })
            .unwrap_or_default();
        predicates.insert(aut_name, preds);
    }

    Ok(Json(ContextPredicatesResponse {
        success: true,
        predicates,
    }))
}

// ---------------------------------------------------------------------------
// Template resolution helper for API handlers
// ---------------------------------------------------------------------------

/// Resolve a formula reference (direct name or template_ref) into a name + an
/// optional synthesized sidecar CTXDSL string.
///
/// - When `formula` is given: returns `(Some(name), None)`.
/// - When `template_ref` is given: instantiates the template, returns
///   `(Some(template_formula_name), Some(synth_sidecar_ctxdsl))` so the caller
///   can pass the synthesized string to `cache::get_or_realize` as an extra
///   sidecar (the cache key then covers the template instantiation, enabling
///   hits across repeated requests with identical template params).
/// - When neither is given: returns `(None, None)` — caller decides whether
///   that's an error.
fn resolve_template_ref_for_cache(
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

    let registry = crate::adapter::templates::TemplateRegistry::builtin();
    let inst = registry
        .instantiate(tref)
        .map_err(|e| ApiError::BadRequest {
            message: format!("Template instantiation failed: {e}"),
            details: None,
        })?;

    let formula_name = inst.name.clone();
    let sidecar_ctxdsl = format!(
        "context __template_sidecar {{\n  mu_formulas {{\n    formula {formula_name} {{\n      over {automaton};\n      body = {};\n    }}\n  }}\n}}\n",
        inst.formula
    );
    Ok((Some(formula_name), Some(sidecar_ctxdsl)))
}

/// Validate an assume/guarantee contract set's discharge graph.
///
/// Mirrors the `mununu contract validate` CLI subcommand. Accepts a
/// `ContractSet` JSON body, runs the §3.x SCC analysis, returns the
/// verdict.
pub async fn contract_validate_handler(
    Json(set): Json<crate::contract::ContractSet>,
) -> ApiResult<Json<crate::contract::discharge::DischargeVerdict>> {
    blocking(move || contract_validate_handler_impl(set)).await
}

fn contract_validate_handler_impl(
    set: crate::contract::ContractSet,
) -> ApiResult<Json<crate::contract::discharge::DischargeVerdict>> {
    let verdict = crate::contract::discharge::validate(&set);
    Ok(Json(verdict))
}

/// Request body for `POST /api/v1/contract/discover`.
#[derive(Debug, serde::Deserialize)]
pub struct ContractDiscoverRequest {
    pub interface: crate::contract::discover::BlackBoxInterface,
    #[serde(default)]
    pub force_controllable: Vec<String>,
    #[serde(default)]
    pub force_uncontrollable: Vec<String>,
    #[serde(default)]
    pub emit_fairness_gap: bool,
    /// Optional contract corpus root used to resolve
    /// `@mununu_interface contract://` URIs on the interface. Mirrors
    /// the CLI's `--corpus` flag for three-surface parity.
    #[serde(default)]
    pub corpus: Option<std::path::PathBuf>,
}

/// Run phase-1 contract discovery on a black-box interface description.
/// Mirrors `mununu contract discover`. The structured `tracing::warn!`
/// diagnostics still fire on the server side; the response carries the
/// full `Phase1Output` (labels + gap markers + corpus resolutions) for
/// the UI to render.
pub async fn contract_discover_handler(
    Json(request): Json<ContractDiscoverRequest>,
) -> ApiResult<Json<crate::contract::discover::Phase1Output>> {
    blocking(move || contract_discover_handler_impl(request)).await
}

fn contract_discover_handler_impl(
    request: ContractDiscoverRequest,
) -> ApiResult<Json<crate::contract::discover::Phase1Output>> {
    use crate::contract::discover::{DiscoverOptions, discover_phase1};
    use crate::corpus::Corpus;

    let force_c: Vec<&str> = request
        .force_controllable
        .iter()
        .map(|s| s.as_str())
        .collect();
    let force_u: Vec<&str> = request
        .force_uncontrollable
        .iter()
        .map(|s| s.as_str())
        .collect();
    let corpus = match &request.corpus {
        Some(root) => Some(Corpus::load(root).map_err(|e| ApiError::BadRequest {
            message: format!("failed to load corpus at {}: {e}", root.display()),
            details: None,
        })?),
        None => None,
    };
    let opts = DiscoverOptions {
        force_controllable: &force_c,
        force_uncontrollable: &force_u,
        emit_fairness_gap: request.emit_fairness_gap,
        corpus: corpus.as_ref(),
    };
    let output = discover_phase1(&request.interface, &opts);
    output.gaps.emit_diagnostics();
    Ok(Json(output))
}

/// Request body for `POST /api/v1/contract/query`.
#[derive(Debug, serde::Deserialize)]
pub struct ContractQueryRequest {
    /// `<domain>/<name>` identifier, e.g. `"rtl_protocol/axi4_slave"`.
    pub id: String,
    /// Filesystem path of the corpus root the server should load.
    pub corpus: std::path::PathBuf,
    /// Parameters to match against. Each value is a JSON value
    /// (number, string, bool, etc.).
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Response from `POST /api/v1/contract/query`. The body carries the
/// ranked candidate list directly.
#[derive(Debug, serde::Serialize)]
pub struct ContractQueryResponse {
    pub candidates: Vec<crate::corpus::ContractEntry>,
}

/// Query the contract corpus (Document D task D2).
///
/// Loads the corpus rooted at `request.corpus`, runs the
/// `(domain, name, parameters)` query, and returns the ranked
/// candidate list.
pub async fn contract_query_handler(
    Json(request): Json<ContractQueryRequest>,
) -> ApiResult<Json<ContractQueryResponse>> {
    blocking(move || contract_query_handler_impl(request)).await
}

fn contract_query_handler_impl(
    request: ContractQueryRequest,
) -> ApiResult<Json<ContractQueryResponse>> {
    let (domain, name) = match request.id.split_once('/') {
        Some((d, n)) if !d.is_empty() && !n.is_empty() => (d.to_string(), n.to_string()),
        _ => {
            return Err(ApiError::BadRequest {
                message: format!(
                    "expected `<domain>/<name>`, got '{}' — example: rtl_protocol/axi4_slave",
                    request.id
                ),
                details: None,
            });
        }
    };

    let corpus = if request.corpus.exists() {
        crate::corpus::Corpus::load(&request.corpus).map_err(|e| ApiError::BadRequest {
            message: format!(
                "failed to load corpus from {}: {e}",
                request.corpus.display()
            ),
            details: None,
        })?
    } else {
        crate::corpus::Corpus::empty()
    };

    let candidates: Vec<crate::corpus::ContractEntry> = corpus
        .query(&domain, &name, &request.parameters)
        .into_iter()
        .cloned()
        .collect();

    Ok(Json(ContractQueryResponse { candidates }))
}

/// Request body for `POST /api/v1/contract/review`.
#[derive(Debug, serde::Deserialize)]
pub struct ContractReviewRequest {
    pub interface: crate::contract::discover::BlackBoxInterface,
    #[serde(default)]
    pub force_controllable: Vec<String>,
    #[serde(default)]
    pub force_uncontrollable: Vec<String>,
    #[serde(default)]
    pub emit_fairness_gap: bool,
    /// Optional contract corpus root used to resolve
    /// `@mununu_interface contract://` URIs into reference proposals.
    #[serde(default)]
    pub corpus: Option<std::path::PathBuf>,
}

/// HITL stage-4 review surface — Document A §A7 / Document D §D.8.
///
/// Wraps phase-2 discovery and adds a flat list of proposed clauses
/// extracted from `@mununu_assume` / `@mununu_guarantee` annotations
/// and resolved corpus references. The approve/edit/reject UX lives in
/// the CLI / UI surfaces.
pub async fn contract_review_handler(
    Json(request): Json<ContractReviewRequest>,
) -> ApiResult<Json<crate::contract::review::ReviewPackage>> {
    blocking(move || contract_review_handler_impl(request)).await
}

fn contract_review_handler_impl(
    request: ContractReviewRequest,
) -> ApiResult<Json<crate::contract::review::ReviewPackage>> {
    use crate::contract::discover::DiscoverOptions;
    use crate::contract::review::build_review_package;
    use crate::corpus::Corpus;

    let force_c: Vec<&str> = request
        .force_controllable
        .iter()
        .map(|s| s.as_str())
        .collect();
    let force_u: Vec<&str> = request
        .force_uncontrollable
        .iter()
        .map(|s| s.as_str())
        .collect();
    let corpus = match &request.corpus {
        Some(root) => Some(Corpus::load(root).map_err(|e| ApiError::BadRequest {
            message: format!("failed to load corpus at {}: {e}", root.display()),
            details: None,
        })?),
        None => None,
    };
    let opts = DiscoverOptions {
        force_controllable: &force_c,
        force_uncontrollable: &force_u,
        emit_fairness_gap: request.emit_fairness_gap,
        corpus: corpus.as_ref(),
    };
    let pkg = build_review_package(&request.interface, &opts);
    pkg.phase1.gaps.emit_diagnostics();
    Ok(Json(pkg))
}

// ============================================================================
// Codesign verify (Document C task C4)
// ============================================================================

/// Request body for `POST /api/v1/codesign/verify`.
#[derive(Debug, serde::Deserialize)]
pub struct CodesignVerifyRequest {
    /// Register-map sidecar contents as a parsed `RegisterMap` value.
    /// HTTP callers should JSON-encode the same shape that the CLI
    /// loads from `register_map.json` on disk.
    pub register_map: crate::codesign::register_map::RegisterMap,
    /// Firmware CTXDSL document text.
    pub firmware_ctxdsl: String,
    /// Formula name to evaluate.
    pub formula: String,
    /// Composition / automaton name to evaluate over. Defaults to the
    /// codesign composition emitted by the splicer
    /// (`<PERIPHERAL>System`).
    #[serde(default)]
    pub automaton: Option<String>,
    /// Optional override for the peripheral automaton name.
    #[serde(default)]
    pub peripheral_automaton: Option<String>,
    /// Optional override for the composition name.
    #[serde(default)]
    pub composition_name: Option<String>,
}

/// Response body for `POST /api/v1/codesign/verify`.
#[derive(Debug, serde::Serialize)]
pub struct CodesignVerifyResponse {
    /// Whether every initial state satisfies the formula.
    pub satisfied: bool,
    /// Total number of states in the composed automaton/composition.
    pub total_states: usize,
    /// Number of states satisfying the formula.
    pub satisfying_states: usize,
    /// Initial state names.
    pub initial_states: Vec<String>,
    /// Subset of `initial_states` that satisfy the formula.
    pub initial_satisfying: Vec<String>,
    /// Composition shape used for evaluation.
    pub composition: CodesignCompositionInfo,
    /// The composed CTXDSL the verifier ran against — useful for the
    /// UI to render alongside the verdict.
    pub composed_ctxdsl: String,
}

/// Composition-shape report for the response.
#[derive(Debug, serde::Serialize)]
pub struct CodesignCompositionInfo {
    pub peripheral_automaton: String,
    pub composition_name: String,
    pub firmware_members: Vec<String>,
    pub automaton: String,
}

/// HW/SW codesign verification handler — Document C task C4.
///
/// Reads a register-map sidecar + firmware CTXDSL, splices the
/// coupling fragment into the firmware document, realises the
/// composed context, and evaluates the named formula. Returns the
/// verdict plus the composed CTXDSL so the UI can render both.
pub async fn codesign_verify_handler(
    Json(request): Json<CodesignVerifyRequest>,
) -> ApiResult<Json<CodesignVerifyResponse>> {
    blocking(move || codesign_verify_handler_impl(request)).await
}

fn codesign_verify_handler_impl(
    request: CodesignVerifyRequest,
) -> ApiResult<Json<CodesignVerifyResponse>> {
    use crate::codesign::compose::{ComposeOptions, compose_codesign_ctxdsl};
    use crate::context_dsl::{parse, realize_context};

    let opts = ComposeOptions {
        peripheral_automaton: request.peripheral_automaton.as_deref(),
        composition_name: request.composition_name.as_deref(),
        firmware_members_override: None,
    };
    let composed = compose_codesign_ctxdsl(&request.register_map, &request.firmware_ctxdsl, &opts)
        .map_err(|e| ApiError::BadRequest {
            message: format!("codesign compose failed: {e}"),
            details: None,
        })?;

    let context_doc = parse(&composed.ctxdsl).map_err(|e| ApiError::Internal {
        message: format!("composed CTXDSL failed to parse: {e:?}"),
        source: None,
    })?;
    let realized = realize_context(&context_doc, &[]).map_err(|e| ApiError::Internal {
        message: format!("composed CTXDSL failed to realise: {e}"),
        source: None,
    })?;

    let formula = realized
        .formulas
        .get(&request.formula)
        .ok_or_else(|| ApiError::BadRequest {
            message: format!("unknown formula '{}' in composed context", request.formula),
            details: None,
        })?;

    let automaton_name = request
        .automaton
        .clone()
        .unwrap_or_else(|| composed.composition_name.clone());
    let clts = realized
        .context
        .clts(&automaton_name)
        .ok_or_else(|| ApiError::BadRequest {
            message: format!(
                "unknown automaton/composition '{automaton_name}' in composed context — expected one of: {}",
                realized.context.clts_names().join(", ")
            ),
            details: None,
        })?;

    let env = realized.environment_for(&automaton_name);
    let options = crate::mu_calculus::EvaluationOptions::default();
    let result = crate::mu_calculus::evaluate_with_options(&formula.formula, clts, &env, &options)
        .map_err(|e| ApiError::Internal {
            message: format!("μ-calculus evaluation failed: {e}"),
            source: None,
        })?;

    let total_states = clts.state_count();
    let satisfying_states = (0..total_states)
        .filter(|i| result.get(*i).map(|b| *b).unwrap_or(false))
        .count();
    let initial_states: Vec<String> = clts
        .initial_states()
        .iter()
        .filter_map(|sid| clts.state_name(*sid).map(str::to_string))
        .collect();
    let initial_satisfying: Vec<String> = clts
        .initial_states()
        .iter()
        .filter_map(|sid| {
            if result.get(sid.index()).map(|b| *b).unwrap_or(false) {
                clts.state_name(*sid).map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    let satisfied = !initial_states.is_empty() && initial_satisfying.len() == initial_states.len();

    Ok(Json(CodesignVerifyResponse {
        satisfied,
        total_states,
        satisfying_states,
        initial_states,
        initial_satisfying,
        composition: CodesignCompositionInfo {
            peripheral_automaton: composed.peripheral_automaton,
            composition_name: composed.composition_name,
            firmware_members: composed.firmware_members,
            automaton: automaton_name,
        },
        composed_ctxdsl: composed.ctxdsl,
    }))
}

// ============================================================================
// Verify project (general N-source verification framework, A2.5)
// ============================================================================

/// Request body for `POST /api/v1/verify`.
///
/// HTTP variant accepts either a pre-parsed `config` JSON object or
/// raw `config_toml` text — the latter is what the UI sends after a
/// user drops a verify.toml file. Source files are read from disk
/// relative to `base_dir`. Inline-content sources (uploading the
/// whole project archive in the request body) remain a future
/// extension.
#[derive(Debug, serde::Deserialize)]
pub struct VerifyProjectRequest {
    /// Pre-parsed `verify.toml` payload. Mutually exclusive with
    /// `config_toml`.
    #[serde(default)]
    pub config: Option<crate::verify::config::VerifyConfig>,
    /// Raw verify.toml text. Parsed via
    /// [`crate::verify::config::VerifyConfig::from_toml`]. Mutually
    /// exclusive with `config`.
    #[serde(default)]
    pub config_toml: Option<String>,
    /// Directory the source paths in the config resolve against.
    /// Required — the server has no implicit "client working
    /// directory" the way the CLI does.
    pub base_dir: String,
    /// R4W-3 (R.4 clustered-COI) — Jaccard similarity floor for the
    /// clustered cone-of-influence comparison the BTOR2 (`sv-yosys`)
    /// route reports on each source's `partition_summary.cluster_coi`.
    /// Overrides any `cluster_similarity_floor` in the supplied config /
    /// config_toml. `None` (default) → the recommended `0.5`. The
    /// comparison itself rides the response's `VerifyReport` (no extra
    /// response field needed).
    #[serde(default)]
    pub cluster_similarity_floor: Option<f64>,
}

/// HTTP handler for the general verify pipeline. Mirrors
/// `mununu verify` (CLI). Returns the structured
/// [`crate::verify::report::VerifyReport`] on success.
///
/// All structural errors (config validation, adapter dispatch
/// failures, parse + realize failures, evaluation failures) come back
/// as 400 Bad Request with the error message.
pub async fn verify_project_handler(
    Json(request): Json<VerifyProjectRequest>,
) -> ApiResult<Json<crate::verify::report::VerifyReport>> {
    blocking(move || verify_project_handler_impl(request)).await
}

fn verify_project_handler_impl(
    request: VerifyProjectRequest,
) -> ApiResult<Json<crate::verify::report::VerifyReport>> {
    let mut config = match (request.config, request.config_toml) {
        (Some(c), None) => c,
        (None, Some(toml_text)) => crate::verify::config::VerifyConfig::from_toml(&toml_text)
            .map_err(|e| ApiError::BadRequest {
                message: format!("failed to parse config_toml: {e}"),
                details: None,
            })?,
        (Some(_), Some(_)) => {
            return Err(ApiError::BadRequest {
                message: "supply exactly one of `config` or `config_toml`, not both".to_string(),
                details: None,
            });
        }
        (None, None) => {
            return Err(ApiError::BadRequest {
                message: "missing `config` or `config_toml` in request body".to_string(),
                details: None,
            });
        }
    };
    // R4W-3 — the request floor overrides any value in the config /
    // config_toml; absent leaves the manifest value (or its None
    // default → 0.5 at the bit-blast layer) in place.
    if request.cluster_similarity_floor.is_some() {
        config.cluster_similarity_floor = request.cluster_similarity_floor;
    }
    let base_dir = std::path::PathBuf::from(&request.base_dir);
    crate::verify::verify_project(&config, &base_dir)
        .map(Json)
        .map_err(|e| ApiError::BadRequest {
            message: e.to_string(),
            details: None,
        })
}

// ============================================================================
// Memory-check (B2b)
// ============================================================================

/// Request body for `POST /api/v1/verify/memory-check`.
///
/// Mirrors [`VerifyProjectRequest`] in shape — either `config` or
/// `config_toml`, never both. The analysis is pure (it inspects only
/// the parsed config), so no `base_dir` is required.
#[derive(Debug, serde::Deserialize)]
pub struct MemoryCheckRequest {
    /// Pre-parsed `verify.toml` payload. Mutually exclusive with
    /// `config_toml`.
    #[serde(default)]
    pub config: Option<crate::verify::config::VerifyConfig>,
    /// Raw verify.toml text. Parsed via
    /// [`crate::verify::config::VerifyConfig::from_toml`]. Mutually
    /// exclusive with `config`.
    #[serde(default)]
    pub config_toml: Option<String>,
}

/// HTTP handler for `mununu memory check`. Returns the structured
/// [`crate::verify::memory_check::MemoryCheckReport`].
///
/// The handler is advisory — warnings appear in the response body
/// but never surface as 4xx. Callers (UI / CI) decide whether to
/// treat warnings as a gate.
pub async fn memory_check_handler(
    Json(request): Json<MemoryCheckRequest>,
) -> ApiResult<Json<crate::verify::memory_check::MemoryCheckReport>> {
    blocking(move || memory_check_handler_impl(request)).await
}

fn memory_check_handler_impl(
    request: MemoryCheckRequest,
) -> ApiResult<Json<crate::verify::memory_check::MemoryCheckReport>> {
    let config = match (request.config, request.config_toml) {
        (Some(c), None) => c,
        (None, Some(toml_text)) => crate::verify::config::VerifyConfig::from_toml(&toml_text)
            .map_err(|e| ApiError::BadRequest {
                message: format!("failed to parse config_toml: {e}"),
                details: None,
            })?,
        (Some(_), Some(_)) => {
            return Err(ApiError::BadRequest {
                message: "supply exactly one of `config` or `config_toml`, not both".to_string(),
                details: None,
            });
        }
        (None, None) => {
            return Err(ApiError::BadRequest {
                message: "missing `config` or `config_toml` in request body".to_string(),
                details: None,
            });
        }
    };
    Ok(Json(crate::verify::memory_check::check_memory_postures(
        &config,
    )))
}

// ============================================================================
// Codesign reconcile-labels (Doc C §C.5 hard gate)
// ============================================================================

/// Request body for `POST /api/v1/codesign/reconcile-labels`.
#[derive(Debug, serde::Deserialize)]
pub struct CodesignReconcileLabelsRequest {
    /// Firmware-side rendezvous labels (the C extraction's alphabet).
    pub firmware_labels: Vec<String>,
    /// Peripheral-side rendezvous labels (the SV extraction's
    /// alphabet — or, today, the alphabet derived from a register
    /// map by `coupling::register_map_labels`).
    pub peripheral_labels: Vec<String>,
}

/// Response body for `POST /api/v1/codesign/reconcile-labels`.
///
/// The handler always returns 200 OK with a populated body — the
/// `mismatch` field distinguishes the two outcomes:
///   - `mismatch == null`: alphabets agree; `shared` holds the
///     canonical sorted alphabet.
///   - `mismatch == { firmware_only, peripheral_only }`: at least one
///     side has labels the other doesn't. `shared` is empty.
#[derive(Debug, serde::Serialize)]
pub struct CodesignReconcileLabelsResponse {
    /// Shared canonical alphabet (sorted), or empty on mismatch.
    pub shared: Vec<String>,
    /// Mismatch report. `None` when the alphabets reconcile exactly.
    pub mismatch: Option<crate::codesign::reconcile::ReconcileMismatch>,
}

/// HW/SW codesign label-alphabet reconcile handler (Doc C §C.5).
///
/// Hard gate against silent over-approximation: refuses to compose
/// firmware ‖ peripheral when the two extractions disagree on the
/// rendezvous-label alphabet. The handler itself returns 200 OK and
/// reports the mismatch in the body; downstream orchestrators (the
/// future `verify-project` flow) consume the `mismatch` field and
/// fail their pipelines on a non-null value.
pub async fn codesign_reconcile_labels_handler(
    Json(request): Json<CodesignReconcileLabelsRequest>,
) -> ApiResult<Json<CodesignReconcileLabelsResponse>> {
    blocking(move || codesign_reconcile_labels_handler_impl(request)).await
}

fn codesign_reconcile_labels_handler_impl(
    request: CodesignReconcileLabelsRequest,
) -> ApiResult<Json<CodesignReconcileLabelsResponse>> {
    use crate::codesign::reconcile::{ReconcileError, reconcile_label_alphabets};
    use std::collections::BTreeSet;

    let firmware: BTreeSet<String> = request.firmware_labels.into_iter().collect();
    let peripheral: BTreeSet<String> = request.peripheral_labels.into_iter().collect();
    match reconcile_label_alphabets(&firmware, &peripheral) {
        Ok(r) => Ok(Json(CodesignReconcileLabelsResponse {
            shared: r.shared,
            mismatch: None,
        })),
        Err(ReconcileError::Mismatch(m)) => Ok(Json(CodesignReconcileLabelsResponse {
            shared: Vec::new(),
            mismatch: Some(m),
        })),
    }
}

/// Request body for `POST /api/v1/codesign/emit-chaotic-stub`.
#[derive(Debug, serde::Deserialize)]
pub struct CodesignEmitChaoticStubRequest {
    /// Parsed register-map JSON sidecar.
    pub register_map: crate::codesign::register_map::RegisterMap,
    /// Optional override for the peripheral automaton name. Defaults
    /// to the uppercased peripheral name from the sidecar. The
    /// context-block name is always `<AutomatonName>ChaoticStub`.
    #[serde(default)]
    pub peripheral_automaton: Option<String>,
    /// When true, refuse to emit and 400 if the register-map
    /// validator reports any issue.
    #[serde(default)]
    pub strict: bool,
}

/// Response body — just the emitted CTXDSL text.
#[derive(Debug, serde::Serialize)]
pub struct CodesignEmitChaoticStubResponse {
    /// The standalone CTXDSL document with its own `context { … }`
    /// wrapper, ready to drop into a `verify.toml` as a `ctxdsl`
    /// source.
    pub ctxdsl: String,
    /// Validation warnings surfaced by the register-map validator.
    /// Empty when the sidecar is well-formed.
    pub warnings: Vec<String>,
}

/// Emit a standalone chaotic-stub CTXDSL document from a register map.
/// Mirrors `mununu codesign emit-chaotic-stub` (CLI).
pub async fn codesign_emit_chaotic_stub_handler(
    Json(request): Json<CodesignEmitChaoticStubRequest>,
) -> ApiResult<Json<CodesignEmitChaoticStubResponse>> {
    blocking(move || codesign_emit_chaotic_stub_handler_impl(request)).await
}

fn codesign_emit_chaotic_stub_handler_impl(
    request: CodesignEmitChaoticStubRequest,
) -> ApiResult<Json<CodesignEmitChaoticStubResponse>> {
    use crate::codesign::coupling::{CouplingOptions, emit_chaotic_stub_ctxdsl};

    let issues = request.register_map.validate();
    let warnings: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
    if request.strict && !warnings.is_empty() {
        return Err(ApiError::BadRequest {
            message: format!("strict mode: {} register-map issue(s)", warnings.len()),
            details: Some(warnings.join("; ")),
        });
    }
    let opts = CouplingOptions {
        peripheral_automaton: request.peripheral_automaton.as_deref(),
        ..Default::default()
    };
    let ctxdsl = emit_chaotic_stub_ctxdsl(&request.register_map, &opts);
    Ok(Json(CodesignEmitChaoticStubResponse { ctxdsl, warnings }))
}

#[cfg(test)]
mod contract_handler_tests {
    use super::*;
    use crate::contract::{
        ClauseKind, ClauseProvenance, ContractClause, ContractSet, DischargeEdge,
        discharge::DischargeVerdict,
    };

    fn clause(id: &str, kind: ClauseKind) -> ContractClause {
        ContractClause {
            id: id.to_string(),
            kind,
            owner: "test".to_string(),
            description: None,
            provenance: ClauseProvenance::UserAuthored,
            mu_rank: None,
        }
    }

    #[tokio::test]
    async fn contract_validate_handler_returns_acyclic_for_linear_set() {
        let set = ContractSet {
            clauses: vec![
                clause("G_a", ClauseKind::Guarantee),
                clause("A_b", ClauseKind::Assumption),
            ],
            discharges: vec![DischargeEdge {
                discharger: "G_a".to_string(),
                dischargee: "A_b".to_string(),
            }],
            environment_assumptions: vec![],
        };
        let Json(verdict) = contract_validate_handler(Json(set)).await.unwrap();
        assert!(matches!(verdict, DischargeVerdict::Acyclic { .. }));
    }

    #[tokio::test]
    async fn contract_validate_handler_returns_circular_for_self_loop() {
        let set = ContractSet {
            clauses: vec![clause("X", ClauseKind::Guarantee)],
            discharges: vec![DischargeEdge {
                discharger: "X".to_string(),
                dischargee: "X".to_string(),
            }],
            environment_assumptions: vec![],
        };
        let Json(verdict) = contract_validate_handler(Json(set)).await.unwrap();
        assert!(matches!(verdict, DischargeVerdict::Circular { .. }));
    }

    #[tokio::test]
    async fn gr1_synthesize_handler_request_grant_realizable() {
        let req = Gr1SynthesizeRequest {
            context: FileContent {
                name: "rg.tlsf".to_string(),
                content: "INFO { TITLE: \"rg\"; DESCRIPTION: \"rg\"; SEMANTICS: Mealy; \
                          TARGET: Mealy; }\nMAIN { INPUTS { req; } OUTPUTS { grant; } \
                          ASSUMPTIONS { G F req; } GUARANTEES { G (req -> F grant); \
                          G (grant -> X !grant); } }"
                    .to_string(),
            },
            adapter: None,
            module: None,
        };
        let Json(resp) = gr1_synthesize_handler(Json(req)).await.unwrap();
        assert!(resp.realizable, "request_grant realizable via the API");
        assert_eq!(resp.monitor_bits, 2);
        let sv = resp.controller_sv.expect("controller SV emitted");
        assert!(sv.contains("module gr1_controller"));
    }
}

#[cfg(test)]
mod controller_mode_tests {
    use super::*;
    use crate::context::ControllerMode;

    #[test]
    fn resolve_returns_projection_by_default() {
        assert_eq!(
            resolve_controller_mode(&None, false).unwrap(),
            ControllerMode::Projection
        );
    }

    #[test]
    fn resolve_legacy_extract_strategy_maps_to_functional() {
        assert_eq!(
            resolve_controller_mode(&None, true).unwrap(),
            ControllerMode::Functional
        );
    }

    #[test]
    fn resolve_explicit_mode_wins_over_extract_strategy() {
        assert_eq!(
            resolve_controller_mode(&Some("projection".into()), true).unwrap(),
            ControllerMode::Projection
        );
    }

    #[test]
    fn resolve_accepts_all_modes_case_insensitive() {
        let cases = [
            ("projection", ControllerMode::Projection),
            ("Functional", ControllerMode::Functional),
            ("PERMISSIVE", ControllerMode::Permissive),
            ("signature-memory", ControllerMode::SignatureMemory),
            ("signature_memory", ControllerMode::SignatureMemory),
            ("SignatureMemory", ControllerMode::SignatureMemory),
            ("product-game", ControllerMode::ProductGame),
            ("parity-game", ControllerMode::ParityGame),
            ("ParityGame", ControllerMode::ParityGame),
        ];
        for (name, expected) in cases {
            assert_eq!(
                resolve_controller_mode(&Some(name.into()), false).unwrap(),
                expected,
                "name `{name}` should map to {expected:?}"
            );
        }
    }

    #[test]
    fn resolve_rejects_unknown_mode() {
        let err = resolve_controller_mode(&Some("strict".into()), false).unwrap_err();
        match err {
            ApiError::BadRequest { message, .. } => {
                assert!(message.contains("Unknown controller_mode"));
                assert!(message.contains("strict"));
            }
            _ => panic!("expected BadRequest, got {err:?}"),
        }
    }
}

#[cfg(test)]
mod composition_modes_handler_tests {
    use super::*;

    /// Smoke test for the new `/api/v1/extraction/composition-modes`
    /// endpoint. Confirms both modes are present, in the expected order,
    /// with non-empty descriptions.
    #[tokio::test]
    async fn composition_modes_handler_returns_both_modes() {
        let response = extraction_composition_modes_handler()
            .await
            .expect("handler should not error");
        let Json(body) = response;
        assert_eq!(body.modes.len(), 2);
        assert_eq!(body.modes[0].name, "synchronous");
        assert_eq!(body.modes[1].name, "asynchronous");
        assert!(!body.modes[0].description.is_empty());
        assert!(!body.modes[1].description.is_empty());
        // The async description carries the soundness caveat — sound for
        // safety / unsound for liveness — which is the key thing UI users
        // need to see when picking a mode.
        assert!(body.modes[1].description.contains("liveness"));
    }
}

#[cfg(all(test, feature = "ast-extract"))]
mod compositional_extract_handler_tests {
    use super::*;
    use crate::api::models::ExtractionExtractRequest;

    /// End-to-end smoke test for the existing `/api/v1/extraction/extract`
    /// endpoint with a compositional config. The endpoint passes the
    /// schema through to `extract_from_source`, so the new
    /// `composition.instances` + `composition.shared` fields flow through
    /// transparently — but a regression here would be invisible without
    /// an explicit test, so we add one.
    #[tokio::test]
    async fn extraction_extract_handler_handles_composition() {
        let config = serde_json::json!({
            "$schema": "extraction_config_v1",
            "domain": "mcp_server",
            "language": "typescript",
            "source": { "file": "test.ts" },
            "targets": [
                {
                    "class": "Worker",
                    "state_fields": ["state"],
                    "methods": { "include": ["save"] }
                }
            ],
            "composition": {
                "type": "asynchronous",
                "name": "race",
                "instances": [
                    { "of": "Worker", "as": "worker_a" },
                    { "of": "Worker", "as": "worker_b" }
                ],
                "shared": ["ev_save"]
            }
        });
        let source = "class Worker {\n    private state: boolean = false;\n    public save(): void { this.state = true; }\n}\n";
        let req = ExtractionExtractRequest {
            config: config.to_string(),
            source: source.to_string(),
            language: Some("typescript".to_string()),
        };
        let response = extraction_extract_handler(Json(req))
            .await
            .expect("handler should succeed");
        let Json(body) = response;
        assert!(body.success);
        // The espec response carries the JSON-serialized
        // ExtractionSpec; deserialize and assert the composition shape.
        let spec: serde_json::Value =
            serde_json::from_str(&body.espec).expect("espec should be valid JSON");
        let automata = spec["model_config"]["automata"]
            .as_array()
            .expect("automata should be an array");
        assert_eq!(automata.len(), 2, "expected 2 instance automata");
        assert_eq!(automata[0]["id"], "worker_a");
        assert_eq!(automata[1]["id"], "worker_b");
        let comp = &spec["model_config"]["composition"];
        assert_eq!(comp["type"], "asynchronous");
        let members = comp["members"]
            .as_array()
            .expect("members should be an array");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], "worker_a");
        assert_eq!(members[1], "worker_b");
    }

    // ---- verify_project_handler --------------------------------------

    #[tokio::test]
    async fn verify_project_handler_accepts_config_toml() {
        let tmp = tempfile::tempdir().unwrap();
        // Minimal hand-authored CTXDSL source — exercises the
        // `ctxdsl` adapter pass-through path so the handler test
        // doesn't depend on clang / yosys / etc.
        let ctxdsl = r#"
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
        std::fs::write(tmp.path().join("light.ctxdsl"), ctxdsl).unwrap();

        let toml = r#"
[project]
name = "ConfigTomlPath"

[[sources]]
id = "light"
adapter = "ctxdsl"
files = ["light.ctxdsl"]

[composition]
semantics = "asynchronous"
members = ["light"]
name = "Sys"

[[properties]]
name = "alive"
formula = "true"
over = "Sys"
"#;
        let request = VerifyProjectRequest {
            config: None,
            config_toml: Some(toml.to_string()),
            base_dir: tmp.path().to_string_lossy().to_string(),
            cluster_similarity_floor: None,
        };
        let Json(report) = verify_project_handler(Json(request))
            .await
            .expect("verify_project should succeed");
        assert_eq!(report.project, "ConfigTomlPath");
        assert_eq!(report.property_verdicts.len(), 1);
        assert!(report.property_verdicts[0].satisfied);
    }

    #[tokio::test]
    async fn verify_project_handler_rejects_both_config_and_config_toml() {
        let cfg = crate::verify::config::VerifyConfig::from_toml(
            r#"
[project]
name = "X"
[[sources]]
id = "x"
adapter = "ctxdsl"
files = ["x.ctxdsl"]
[composition]
semantics = "asynchronous"
members = ["x"]
"#,
        )
        .unwrap();
        let request = VerifyProjectRequest {
            config: Some(cfg),
            config_toml: Some("[project]\nname = \"Y\"\n".to_string()),
            base_dir: ".".to_string(),
            cluster_similarity_floor: None,
        };
        let err = verify_project_handler(Json(request)).await.unwrap_err();
        match err {
            ApiError::BadRequest { message, .. } => {
                assert!(message.contains("exactly one"), "got: {message}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn context_import_handler_accepts_crewai_format() {
        let crew = r#"{
            "name": "Mini",
            "agents": [{ "role": "Solo" }],
            "tasks": [{ "agent": "Solo" }]
        }"#;
        let request = ContextImportRequest {
            content: crew.to_string(),
            format: "crewai".to_string(),
            filename: Some("mini.crewai.json".to_string()),
            sidecar: None,
            additional_sources: Vec::new(),
            use_sv2v: false,
            ..Default::default()
        };
        let Json(out) = context_import_handler(Json(request))
            .await
            .expect("crewai dispatch should succeed");
        assert!(out.ctxdsl.contains("Agent_Solo"));
        // CrewAI currently rides the XState SourceFormat variant until
        // `SourceFormat::Crewai` lands.
        assert_eq!(out.source_format, "XState");
    }

    #[tokio::test]
    async fn context_import_handler_accepts_langgraph_format() {
        let graph = r#"{
            "name": "Linear",
            "entry_point": "a",
            "nodes": [
                { "id": "a" },
                { "id": "b" }
            ],
            "edges": [{ "from": "a", "to": "b" }]
        }"#;
        let request = ContextImportRequest {
            content: graph.to_string(),
            format: "langgraph".to_string(),
            filename: Some("graph.langgraph.json".to_string()),
            sidecar: None,
            additional_sources: Vec::new(),
            use_sv2v: false,
            ..Default::default()
        };
        let Json(out) = context_import_handler(Json(request))
            .await
            .expect("langgraph dispatch should succeed");
        assert!(out.ctxdsl.contains("automaton Linear"));
    }

    /// R.6.7 / V.6 (2026-06-09) — API surface for the
    /// controllability-aware predicate-cube lift. When the request
    /// declares `predicates` + `controllable_inputs` + `format == "btor2"`,
    /// the handler routes through `predicate_cube_lift` + returns
    /// a summary CTXDSL + the lift's `AdapterWarning`s + a summary
    /// line counting cube / mayonly / sharp / hyper-must / env-label /
    /// ctrl-label counts.
    #[tokio::test]
    async fn context_import_handler_routes_through_controllability_aware_lift() {
        use crate::api::models::PredicateSpecRequest;
        // V.6 AMBA arbiter BTOR2 (inline; matches
        // examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2).
        let btor2 = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 req_0
4 input 1 req_1
5 input 1 ctrl_g0
6 input 1 ctrl_g1
7 state 2 burst
8 state 1 grant_0
9 state 1 grant_1
10 zero 1
11 zero 2
12 init 1 8 10
13 init 1 9 10
14 init 2 7 11
15 one 1
16 const 2 11
17 const 2 01
18 const 2 10
19 const 2 00
20 next 1 8 5
21 next 1 9 6
22 or 1 5 6
23 eq 1 7 19
24 sub 2 7 17
25 ite 2 23 16 24
26 ite 2 22 25 7
27 next 2 7 26
";

        let request = ContextImportRequest {
            content: btor2.to_string(),
            format: "btor2".to_string(),
            filename: Some("amba_arbiter.btor2".to_string()),
            sidecar: None,
            additional_sources: Vec::new(),
            use_sv2v: false,
            predicates: vec![PredicateSpecRequest {
                name: "burst_zero".to_string(),
                register: "burst".to_string(),
                value: 0,
            }],
            controllable_inputs: vec!["ctrl_g0".to_string(), "ctrl_g1".to_string()],
            sv_source_path: None,
            sidecar_path: None,
        };

        let Json(out) = context_import_handler(Json(request))
            .await
            .expect("V.6 controllability-aware lift via API should succeed");

        // The handler returns a summary CTXDSL + the lift's
        // [R.6.7 V.6 ...] summary line in `warnings`.
        assert!(
            out.ctxdsl
                .contains("R.6.7 / V.6 controllability-aware lift summary"),
            "summary CTXDSL must contain the V.6 marker; got: {}",
            out.ctxdsl
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("[R.6.7 V.6 controllability-aware lift]")),
            "warnings must contain the lift summary line; got: {:?}",
            out.warnings
        );
        // Single predicate ⇒ 2 cubes (matches `cube_count` in the
        // existing V.6 integration test
        // `v6_amba_arbiter_lift_produces_expected_cube_count`).
        assert_eq!(out.state_count, 2);
    }

    /// U.0 (slot 6) — the `/api/v1/btor2/cegar` endpoint runs the CEGAR
    /// loop end-to-end and returns the refinement trace the UI viewer
    /// renders: per-iteration records + a terminated_with reason + the
    /// final 3-valued verdict cell counts.
    #[tokio::test]
    async fn btor2_cegar_handler_returns_refinement_trace() {
        use crate::api::models::PredicateSpecRequest;
        // Reuse the V.6 AMBA arbiter (a burst counter → MayOnly edges
        // under the `burst==0` predicate abstraction).
        let btor2 = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 req_0
4 input 1 req_1
5 input 1 ctrl_g0
6 input 1 ctrl_g1
7 state 2 burst
8 state 1 grant_0
9 state 1 grant_1
10 zero 1
11 zero 2
12 init 1 8 10
13 init 1 9 10
14 init 2 7 11
15 one 1
16 const 2 11
17 const 2 01
18 const 2 10
19 const 2 00
20 next 1 8 5
21 next 1 9 6
22 or 1 5 6
23 eq 1 7 19
24 sub 2 7 17
25 ite 2 23 16 24
26 ite 2 22 25 7
27 next 2 7 26
";

        let request = Btor2CegarRequest {
            content: btor2.to_string(),
            formula: "nu X. < true > X".to_string(),
            predicates: vec![PredicateSpecRequest {
                name: "burst_zero".to_string(),
                register: "burst".to_string(),
                value: 0,
            }],
            controllable_inputs: vec!["ctrl_g0".to_string(), "ctrl_g1".to_string()],
            predicate_source: None,
            max_iterations: Some(4),
            must_edge_inference: None,
            may_edge_inference: None,
            config_values: vec![],
            emit_ctxdsl: false,
            engine: None,
        };

        let Json(out) = btor2_cegar_handler(Json(request))
            .await
            .expect("CEGAR endpoint should run end-to-end");

        assert!(out.success);
        // iteration 0 is always present (the initial evaluation).
        assert!(
            !out.iterations.is_empty(),
            "trace must record at least the initial iteration"
        );
        assert_eq!(out.iterations[0].iteration, 0);
        assert!(
            matches!(
                out.terminated_with.as_str(),
                "converged" | "bounded-iterations-reached" | "predicate-source-exhausted"
            ),
            "unexpected terminated_with: {}",
            out.terminated_with
        );
        // Single predicate ⇒ 2 cubes; the verdict summary covers every cube.
        let total = out.verdict.true_cells + out.verdict.false_cells + out.verdict.unknown_cells;
        assert_eq!(
            total, 2,
            "verdict summary must cover both cubes; got {total}"
        );
        // The final predicate set includes at least the bootstrap predicate.
        assert!(!out.final_predicates.is_empty());
        // CTXDSL Phase 2 — emit_ctxdsl defaulted false ⇒ no ctxdsl field.
        assert!(
            out.ctxdsl.is_none(),
            "ctxdsl must be omitted when emit_ctxdsl=false"
        );
    }

    /// R-F5.4.2b + R-F5.5b — `engine: "symbolic"` runs the R-F5 BDD engine with
    /// the symbolic CEGAR loop and returns the standard [`Btor2CegarResponse`]
    /// shape: per-iteration records, a `terminated_with` reason, and the
    /// `{T,F,⊥}` verdict over the feasible cubes. `EF(cnt==0)` on this
    /// monotone counter is definite everywhere (T at cnt==0, F elsewhere), so
    /// it converges at iteration 0.
    #[tokio::test]
    async fn btor2_cegar_handler_symbolic_engine_cegar() {
        use crate::api::models::PredicateSpecRequest;
        // 2-bit saturating counter with an enable.
        let btor2 = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 input 2 en
5 one 1
6 ones 1
7 add 1 3 5
8 eq 2 3 6
9 ite 1 8 3 7
10 ite 1 4 9 3
11 next 1 3 10
";
        let request = Btor2CegarRequest {
            content: btor2.to_string(),
            formula: "mu X. p or <> X".to_string(), // EF (cnt==0), bare diamond
            predicates: vec![PredicateSpecRequest {
                name: "p".to_string(),
                register: "cnt".to_string(),
                value: 0,
            }],
            controllable_inputs: vec![],
            predicate_source: None,
            max_iterations: None,
            must_edge_inference: None,
            may_edge_inference: None,
            config_values: vec![],
            emit_ctxdsl: false,
            engine: Some("symbolic".to_string()),
        };

        let Json(out) = btor2_cegar_handler(Json(request))
            .await
            .expect("symbolic CEGAR endpoint should run");

        assert!(out.success);
        // The loop records at least the initial evaluation (iteration 0).
        assert!(!out.iterations.is_empty(), "loop records iteration 0");
        assert_eq!(out.iterations[0].iteration, 0);
        // Definite everywhere ⇒ converged, no ⊥.
        assert_eq!(out.terminated_with, "converged");
        assert_eq!(out.verdict.unknown_cells, 0);
        // One predicate (cnt==0) ⇒ 2 feasible cubes ({cnt==0}, {cnt!=0}).
        let total = out.verdict.true_cells + out.verdict.false_cells + out.verdict.unknown_cells;
        assert_eq!(total, 2, "verdict covers both feasible cubes; got {total}");
        assert!(out.verdict.true_cells >= 1);
        assert!(
            out.warnings.iter().any(|w| w.contains("symbolic")),
            "symbolic path surfaces its caveat"
        );
    }

    /// CTXDSL Phase 2 (2026-06-22) — `emit_ctxdsl: true` returns the final
    /// refined cube model + the checked formula as a self-contained CTXDSL
    /// document in the response's `ctxdsl` field.
    #[tokio::test]
    async fn btor2_cegar_handler_emit_ctxdsl_returns_model_and_formula() {
        use crate::api::models::PredicateSpecRequest;
        let btor2 = "\
1 sort bitvec 2
2 state 2 burst
3 zero 2
4 init 2 2 3
5 const 2 01
6 sub 2 2 5
7 next 2 2 6
";
        let request = Btor2CegarRequest {
            content: btor2.to_string(),
            formula: "nu X. ([] X)".to_string(),
            predicates: vec![PredicateSpecRequest {
                name: "burst_zero".to_string(),
                register: "burst".to_string(),
                value: 0,
            }],
            controllable_inputs: vec![],
            predicate_source: None,
            max_iterations: Some(2),
            must_edge_inference: None,
            may_edge_inference: None,
            config_values: vec![],
            emit_ctxdsl: true,
            engine: None,
        };
        let Json(out) = btor2_cegar_handler(Json(request))
            .await
            .expect("CEGAR endpoint runs with emit_ctxdsl");
        let ctxdsl = out
            .ctxdsl
            .expect("emit_ctxdsl=true ⇒ response carries the model CTXDSL");
        assert!(ctxdsl.contains("context "), "ctxdsl:\n{ctxdsl}");
        assert!(
            ctxdsl.contains("automaton lifted_kmts {"),
            "ctxdsl:\n{ctxdsl}"
        );
        assert!(ctxdsl.contains("mu_formulas {"), "ctxdsl:\n{ctxdsl}");
        assert!(
            ctxdsl.contains("formula checked_property {"),
            "ctxdsl:\n{ctxdsl}"
        );
        assert!(
            ctxdsl.contains("body = nu X. ([] X);"),
            "formula body must round-trip verbatim; ctxdsl:\n{ctxdsl}"
        );
    }

    /// M.6 parity (2026-06-20) — the CEGAR endpoint now accepts `config_values`
    /// (R-S8 symbolic init) and `may_edge_inference`, both previously CLI-only.
    /// A well-formed request with both set runs end-to-end.
    #[tokio::test]
    async fn btor2_cegar_handler_accepts_config_values_and_may_edge() {
        use crate::api::models::PredicateSpecRequest;
        let btor2 = "\
1 sort bitvec 2
2 state 2 burst
3 zero 2
4 init 2 2 3
5 const 2 01
6 sub 2 2 5
7 next 2 2 6
";
        let request = Btor2CegarRequest {
            content: btor2.to_string(),
            formula: "nu X. < true > X".to_string(),
            predicates: vec![PredicateSpecRequest {
                name: "burst_zero".to_string(),
                register: "burst".to_string(),
                value: 0,
            }],
            controllable_inputs: vec![],
            predicate_source: None,
            max_iterations: Some(4),
            must_edge_inference: None,
            may_edge_inference: Some("smt-all-pairs".to_string()),
            config_values: vec!["burst=0,1,2,3".to_string()],
            emit_ctxdsl: false,
            engine: None,
        };
        let Json(out) = btor2_cegar_handler(Json(request))
            .await
            .expect("CEGAR endpoint should accept config_values + may_edge_inference");
        assert!(out.success);
        assert!(!out.iterations.is_empty());
    }

    /// M.6 parity — a malformed `config_values` entry is a 400, not a panic.
    #[tokio::test]
    async fn btor2_cegar_handler_rejects_malformed_config_values() {
        use crate::api::models::PredicateSpecRequest;
        let btor2 = "1 sort bitvec 1\n2 state 1 r\n3 zero 1\n4 init 1 2 3\n5 next 1 2 3\n";
        let request = Btor2CegarRequest {
            content: btor2.to_string(),
            formula: "nu X. < true > X".to_string(),
            predicates: vec![PredicateSpecRequest {
                name: "r_zero".to_string(),
                register: "r".to_string(),
                value: 0,
            }],
            controllable_inputs: vec![],
            predicate_source: None,
            max_iterations: Some(2),
            must_edge_inference: None,
            may_edge_inference: None,
            config_values: vec!["this is not valid".to_string()],
            emit_ctxdsl: false,
            engine: None,
        };
        let err = btor2_cegar_handler(Json(request))
            .await
            .expect_err("malformed config_values must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    /// mununu#459 — a `sv verify-auto` `config_values` entry whose VALUE is not a decimal u64
    /// (e.g. hex `0x1`) is a 400 that names the value, returned BEFORE the (heavy) lift — never a
    /// silent drop that verifies a different question than the one asked. (Unknown / non-input
    /// SIGNAL names are caught downstream against the model's real inputs; see
    /// `verify_auto::tests::config_pins_partition_applied_vs_unusable`.)
    #[test]
    fn sv_verify_auto_handler_rejects_hex_config_value() {
        use crate::api::models::SvVerifyAutoRequest;
        let request: SvVerifyAutoRequest = serde_json::from_value(serde_json::json!({
            "source": "module m(input logic clk, input logic rst); endmodule\n",
            "config_values": ["rst=0x1"],
        }))
        .expect("request deserializes");
        let err = sv_verify_auto_handler_impl(request)
            .expect_err("a hex config_values value must be rejected, not silently dropped");
        match err {
            ApiError::BadRequest { details, .. } => assert!(
                details.as_deref().is_some_and(|d| d.contains("decimal")),
                "the 400 must explain the value is not a decimal u64: {details:?}"
            ),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    /// cegar-extraction Stage 2 — the SV-direct CEGAR endpoint rejects an
    /// empty predicate set BEFORE invoking the (heavy) sv2v+Yosys lift, so
    /// the guard is testable without the toolchain present.
    #[tokio::test]
    async fn sv_cegar_handler_rejects_empty_predicates() {
        let request = SvCegarRequest {
            source: "module m(input logic clk); endmodule\n".to_string(),
            additional_sources: vec![],
            top: None,
            use_sv2v: false,
            setundef_anyseq: false,
            setundef_anyconst: false,
            formula: "nu X. < true > X".to_string(),
            predicates: vec![],
            controllable_inputs: vec![],
            predicate_source: None,
            max_iterations: Some(2),
            must_edge_inference: None,
            may_edge_inference: None,
            config_values: vec![],
            emit_ctxdsl: false,
            engine: None,
        };
        let err = sv_cegar_handler(Json(request))
            .await
            .expect_err("empty predicates must be rejected before the lift");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    /// cegar-extraction Stage 2 — the SV-direct CEGAR endpoint lifts a real
    /// SV design (sv2v + Yosys → flattened BTOR2) and runs the CEGAR loop
    /// end-to-end, returning the same trace shape as `/btor2/cegar`. Gated
    /// on the yosys toolchain (skips when absent, matching the yosys-test
    /// convention).
    #[tokio::test]
    async fn sv_cegar_handler_lifts_sv_and_runs_cegar() {
        // Skip when yosys is not on PATH (CI-without-toolchain).
        if std::process::Command::new("yosys")
            .arg("-V")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skip: yosys not installed");
            return;
        }
        // A 2-bit down-counter register `burst`; clk/rst become inputs.
        let sv = "\
module ctr (input logic clk, input logic rst);
  logic [1:0] burst;
  always_ff @(posedge clk) begin
    if (rst) burst <= 2'd0;
    else burst <= burst - 2'd1;
  end
endmodule
";
        let request = SvCegarRequest {
            source: sv.to_string(),
            additional_sources: vec![],
            top: Some("ctr".to_string()),
            use_sv2v: false,
            setundef_anyseq: false,
            setundef_anyconst: false,
            formula: "nu X. < true > X".to_string(),
            predicates: vec![PredicateSpecRequest {
                name: "burst_zero".to_string(),
                register: "burst".to_string(),
                value: 0,
            }],
            controllable_inputs: vec![],
            predicate_source: None,
            max_iterations: Some(4),
            must_edge_inference: None,
            may_edge_inference: None,
            config_values: vec![],
            emit_ctxdsl: false,
            engine: None,
        };
        let Json(out) = sv_cegar_handler(Json(request))
            .await
            .expect("SV-direct CEGAR should lift + run end-to-end");
        assert!(out.success);
        assert!(
            !out.iterations.is_empty(),
            "trace must record at least the initial iteration"
        );
        assert_eq!(out.iterations[0].iteration, 0);
        // Single predicate ⇒ 2 cubes; the verdict covers every cube.
        let total = out.verdict.true_cells + out.verdict.false_cells + out.verdict.unknown_cells;
        assert_eq!(total, 2, "verdict must cover both cubes; got {total}");
        assert!(!out.final_predicates.is_empty());
    }

    /// R.6.7 / V.6 — when only `predicates` is set (no
    /// `controllable_inputs`), the controllability-aware path does
    /// not fire; the legacy BTOR2 adapter runs.
    #[tokio::test]
    async fn context_import_handler_skips_v6_path_without_controllable_inputs() {
        use crate::api::models::PredicateSpecRequest;
        let btor2 = "1 sort bitvec 1\n2 state 1 reg_a\n3 zero 1\n4 init 1 2 3\n5 next 1 2 3\n";
        let request = ContextImportRequest {
            content: btor2.to_string(),
            format: "btor2".to_string(),
            filename: Some("trivial.btor2".to_string()),
            sidecar: None,
            additional_sources: Vec::new(),
            use_sv2v: false,
            predicates: vec![PredicateSpecRequest {
                name: "p".to_string(),
                register: "reg_a".to_string(),
                value: 0,
            }],
            controllable_inputs: Vec::new(), // <-- empty disables V.6 path
            sv_source_path: None,
            sidecar_path: None,
        };
        let Json(out) = context_import_handler(Json(request))
            .await
            .expect("legacy BTOR2 import should succeed");
        // Legacy BTOR2 adapter output — no V.6 summary marker.
        assert!(
            !out.ctxdsl
                .contains("R.6.7 / V.6 controllability-aware lift summary"),
            "V.6 marker must NOT appear when controllable_inputs is empty; got: {}",
            out.ctxdsl
        );
    }

    #[tokio::test]
    async fn verify_project_handler_rejects_missing_config_and_toml() {
        let request = VerifyProjectRequest {
            config: None,
            config_toml: None,
            base_dir: ".".to_string(),
            cluster_similarity_floor: None,
        };
        let err = verify_project_handler(Json(request)).await.unwrap_err();
        match err {
            ApiError::BadRequest { message, .. } => {
                assert!(message.contains("missing"), "got: {message}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    // ---- memory_check_handler -----------------------------------------

    #[tokio::test]
    async fn memory_check_handler_accepts_config_toml_and_surfaces_warnings() {
        let toml_text = r#"
[project]
name = "MC"

[[sources]]
id = "mem"
adapter = "ctxdsl"
files = ["m.ctxdsl"]

[sources.memory_abstraction]
kind = "tracked_addresses"
tracked = ["x"]

[composition]
semantics = "asynchronous"
members = ["mem"]

[[properties]]
name = "p"
formula = "mem.x.fresh"
"#;
        let request = MemoryCheckRequest {
            config: None,
            config_toml: Some(toml_text.to_string()),
        };
        let Json(report) = memory_check_handler(Json(request))
            .await
            .expect("memory check should succeed");
        assert_eq!(report.postures.len(), 1);
        assert!(report.warnings.iter().any(|w| matches!(
            w,
            crate::verify::memory_check::MemoryCheckWarning::ValueMentionOnTrackedAddressesPosture { .. }
        )));
    }

    #[tokio::test]
    async fn memory_check_handler_rejects_both_config_and_config_toml() {
        let cfg = crate::verify::config::VerifyConfig::from_toml(
            r#"
[project]
name = "X"
[[sources]]
id = "x"
adapter = "ctxdsl"
files = ["x.ctxdsl"]
[composition]
semantics = "asynchronous"
members = ["x"]
"#,
        )
        .unwrap();
        let request = MemoryCheckRequest {
            config: Some(cfg),
            config_toml: Some("ignored".to_string()),
        };
        let err = memory_check_handler(Json(request)).await.unwrap_err();
        match err {
            ApiError::BadRequest { message, .. } => {
                assert!(message.contains("exactly one"), "got: {message}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_check_handler_rejects_missing_config_and_toml() {
        let request = MemoryCheckRequest {
            config: None,
            config_toml: None,
        };
        let err = memory_check_handler(Json(request)).await.unwrap_err();
        match err {
            ApiError::BadRequest { message, .. } => {
                assert!(message.contains("missing"), "got: {message}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    /// The safety-portfolio endpoint decides a small reachable counter (the exact
    /// engine is enough — btormc/Pono may be absent) and reports the deciding engine.
    #[tokio::test]
    async fn btor2_verify_handler_decides_reachable_counter() {
        // 3-bit counter incrementing to `ones`; `bad = (c == 7)` is reachable.
        let btor2 = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                     6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                     10 eq 9 3 8\n11 bad 10\n";
        let request = Btor2VerifyRequest {
            content: btor2.to_string(),
        };
        let Json(out) = btor2_verify_handler(Json(request))
            .await
            .expect("verify runs");
        // Canonical verdict: `bad` reachable ⇒ the safety property is VIOLATED. The
        // reachability detail (which engine found it) stays in `reachable_by`.
        assert_eq!(out.verdict, "violated", "outcome: {out:?}");
        assert!(
            out.reachable_by.contains(&"exact".to_string()),
            "the exact engine should decide the small reachable counter: {out:?}"
        );
        assert!(
            !out.contradiction,
            "no sound engine should disagree: {out:?}"
        );
    }

    /// Malformed BTOR2 is a `BadRequest`, not a 500 — the parse guard runs before
    /// any engine work.
    #[tokio::test]
    async fn btor2_verify_handler_rejects_malformed_btor2() {
        let request = Btor2VerifyRequest {
            content: "this is not btor2".to_string(),
        };
        let err = btor2_verify_handler(Json(request))
            .await
            .expect_err("malformed BTOR2 must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    // A 4-state staller: st 0=idle,1=req,3=stuck; 2=grant unreachable. req -> stuck ->
    // stuck forever ⇒ AG((st==1) → AF (st==2)) is VIOLATED.
    const LIVENESS_STALLER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 ite 1 6 7 4
13 ite 1 11 9 3
14 ite 1 10 12 13
15 next 1 3 14
";

    #[tokio::test]
    async fn btor2_verify_liveness_handler_decides_violated_staller() {
        let request = Btor2VerifyLivenessRequest {
            content: LIVENESS_STALLER.to_string(),
            request: "st == 1".to_string(),
            grant: "st == 2".to_string(),
        };
        let Json(out) = btor2_verify_liveness_handler(Json(request))
            .await
            .expect("verify-liveness runs");
        assert_eq!(out.verdict, "violated", "response: {out:?}");
        assert!(out.property.contains("AF"), "property echoed: {out:?}");
    }

    #[tokio::test]
    async fn btor2_verify_liveness_handler_rejects_relational_atom() {
        let request = Btor2VerifyLivenessRequest {
            content: LIVENESS_STALLER.to_string(),
            request: "st == 1".to_string(),
            grant: "x == y".to_string(), // relational — out of the response fragment
        };
        let err = btor2_verify_liveness_handler(Json(request))
            .await
            .expect_err("relational atom must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    #[tokio::test]
    async fn btor2_verify_liveness_all_handler_decides_violated_conjunction() {
        // The staller violates `st == 1 => st == 2`; a conjunction containing it is
        // violated regardless of the second (trivially-holding) conjunct.
        let request = Btor2VerifyLivenessAllRequest {
            content: LIVENESS_STALLER.to_string(),
            responses: vec![
                "st == 1 => st == 2".to_string(),
                "st == 0 => st == 0".to_string(),
            ],
        };
        let Json(out) = btor2_verify_liveness_all_handler(Json(request))
            .await
            .expect("verify-liveness-all runs");
        assert_eq!(out.verdict, "violated", "response: {out:?}");
        assert!(out.property.contains("&&"), "conjunction echoed: {out:?}");
    }

    #[tokio::test]
    async fn btor2_verify_liveness_all_handler_rejects_missing_arrow() {
        let request = Btor2VerifyLivenessAllRequest {
            content: LIVENESS_STALLER.to_string(),
            responses: vec!["st == 1 st == 2".to_string()], // no `=>`
        };
        let err = btor2_verify_liveness_all_handler(Json(request))
            .await
            .expect_err("a response without `=>` must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    // The staller's `stuck` state is an absorbing trap ⇒ AG EF (st==0) is VIOLATED
    // (from stuck you can never get back to idle).
    #[tokio::test]
    async fn btor2_verify_recoverability_handler_detects_absorbing_trap() {
        let request = Btor2VerifyRecoverabilityRequest {
            content: LIVENESS_STALLER.to_string(),
            target: "st == 0".to_string(),
            predicates: Vec::new(),
            refine: false,
            config_values: Vec::new(),
            discover_assumptions: false,
        };
        let Json(out) = btor2_verify_recoverability_handler(Json(request))
            .await
            .expect("verify-recoverability runs");
        assert_eq!(out.verdict, "violated", "response: {out:?}");
        assert_eq!(out.property, "AG EF (st == 0)");
        assert!(out.refinement.is_none(), "no refinement without `refine`");
    }

    /// `refine: true` carries a structured refinement alongside the verdict (refined-verdicts
    /// Phase 0). A stuck-at-0 flag can never recover to 1, so the target is flagged `vacuous`.
    #[tokio::test]
    async fn btor2_verify_recoverability_refine_flags_vacuous_target() {
        const STUCK: &str =
            "1 sort bitvec 1\n2 state 1 flag\n3 zero 1\n4 init 1 2 3\n5 next 1 2 3\n";
        let request = Btor2VerifyRecoverabilityRequest {
            content: STUCK.to_string(),
            target: "flag == 1".to_string(),
            predicates: Vec::new(),
            refine: true,
            config_values: Vec::new(),
            discover_assumptions: false,
        };
        let Json(out) = btor2_verify_recoverability_handler(Json(request))
            .await
            .expect("verify-recoverability --refine runs");
        assert_ne!(out.verdict, "holds");
        let refinement = out.refinement.expect("refine returns a refinement");
        let vac = refinement.vacuous.expect("unreachable target is vacuous");
        assert!(vac.good_unreachable);
    }

    /// `config_values` carries a config-partition (refined-verdicts capability A): `busy` recovers
    /// unless `mode == 3`, so the partition holds for mode ∈ {0,1,2} and violates for mode == 3.
    #[tokio::test]
    async fn btor2_verify_recoverability_config_values_partitions_the_verdict() {
        const MODE_DEP: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 start\n4 input 2 mode\n\
5 state 1 busy\n6 zero 1\n7 init 1 5 6\n8 const 2 11\n9 eq 1 4 8\n10 not 1 5\n11 and 1 3 10\n\
12 and 1 5 9\n13 or 1 11 12\n14 next 1 5 13\n";
        let request = Btor2VerifyRecoverabilityRequest {
            content: MODE_DEP.to_string(),
            target: "busy == 0".to_string(),
            predicates: Vec::new(),
            refine: false,
            config_values: vec!["mode=0,1,2,3".to_string()],
            discover_assumptions: false,
        };
        let Json(out) = btor2_verify_recoverability_handler(Json(request))
            .await
            .expect("verify-recoverability --config-values runs");
        let part = out
            .refinement
            .and_then(|r| r.config_partition)
            .expect("config_values yields a config_partition");
        assert_eq!(part.holds.len(), 3, "mode ∈ {{0,1,2}} HOLD");
        assert_eq!(part.violated, vec![vec![("mode".to_string(), 3)]]);
    }

    #[tokio::test]
    async fn btor2_verify_recoverability_discover_assumptions_finds_enabling_hold() {
        // IDLE(0) --go--> WORK(1); WORK --en=0--> FAULT(2, absorbing) / --en=1--> IDLE. Free-input the
        // FAULT trap is reachable ⇒ VIOLATED; held `en==1` keeps it unreachable ⇒ a non-vacuous HOLDS.
        const EN_TRAP: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 input 2 en\n4 input 2 go\n\
5 state 1 st\n6 zero 1\n7 init 1 5 6\n8 one 1\n9 constd 1 2\n10 eq 2 5 6\n11 eq 2 5 8\n\
13 ite 1 4 8 6\n14 ite 1 3 6 9\n15 ite 1 11 14 9\n16 ite 1 10 13 15\n17 next 1 5 16\n";
        let request = Btor2VerifyRecoverabilityRequest {
            content: EN_TRAP.to_string(),
            target: "st == 0".to_string(),
            predicates: Vec::new(),
            refine: false,
            config_values: Vec::new(),
            discover_assumptions: true,
        };
        let Json(out) = btor2_verify_recoverability_handler(Json(request))
            .await
            .expect("verify-recoverability --discover-assumptions runs");
        assert_eq!(
            out.verdict, "violated",
            "canonical verdict is unchanged (conditional-only)"
        );
        let holds_under = out.refinement.map(|r| r.holds_under).unwrap_or_default();
        assert!(
            holds_under
                .iter()
                .any(|a| a.phi == "en == 1" && a.non_vacuous),
            "discovers the enabling assumption en == 1: {holds_under:?}"
        );
    }

    #[tokio::test]
    async fn btor2_verify_recoverability_handler_rejects_malformed_target() {
        let request = Btor2VerifyRecoverabilityRequest {
            content: LIVENESS_STALLER.to_string(),
            target: "definitely not an atom".to_string(),
            predicates: Vec::new(),
            refine: false,
            config_values: Vec::new(),
            discover_assumptions: false,
        };
        let err = btor2_verify_recoverability_handler(Json(request))
            .await
            .expect_err("malformed target must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    // A 3-bit sparse FSM (enum Idle=1, Busy=2, Done=4) with a COMPUTED illegal-encoding
    // bug: from Busy, `go` assigns `st + 3` (= 5 = 3'b101), outside the enum. The
    // auto-scan discovers `st`, derives legal {1,2,4}, and reports the reachable illegal
    // encoding 5 — no user input.
    const ILLEGAL_ENCODING_FSM: &str = "\
1 sort bitvec 3
2 sort bitvec 1
3 state 1 st
4 constd 1 1
5 init 1 3 4
6 input 2 go
7 constd 1 2
8 constd 1 4
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 eq 2 3 8
13 add 1 3 9
14 ite 1 6 13 8
15 ite 1 6 7 4
16 ite 1 12 4 4
17 ite 1 11 14 16
18 ite 1 10 15 17
19 next 1 3 18
";

    #[tokio::test]
    async fn btor2_check_fsm_handler_reports_the_illegal_encoding() {
        let request = Btor2CheckFsmRequest {
            content: ILLEGAL_ENCODING_FSM.to_string(),
            max_width: crate::adapter::fsm_scan::DEFAULT_FSM_MAX_WIDTH,
        };
        let Json(out) = btor2_check_fsm_handler(Json(request))
            .await
            .expect("check-fsm runs");
        assert_eq!(out.fsm_registers_checked, 1, "response: {out:?}");
        assert_eq!(out.illegal_encodings_found, 1);
        let st = &out.registers[0];
        assert_eq!(st.register, "st");
        assert_eq!(st.legal_encodings, vec![1, 2, 4]);
        assert_eq!(st.verdict, "violated");
        assert!(st.illegal_encoding_reachable);
    }

    #[tokio::test]
    async fn btor2_check_fsm_handler_rejects_malformed_btor2() {
        let request = Btor2CheckFsmRequest {
            content: "this is not btor2".to_string(),
            max_width: crate::adapter::fsm_scan::DEFAULT_FSM_MAX_WIDTH,
        };
        let err = btor2_check_fsm_handler(Json(request))
            .await
            .expect_err("malformed BTOR2 must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    // Two-player game verb: `st' = c` (controller wins) and `st' = c & ¬e` (environment blocks) — the
    // same known-answer 1-bit games as tests/exact_two_player_strategy.rs, over the HTTP handler.
    const GAME_CTRL: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 next 1 4 2\n";
    const GAME_ENVBLK: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 not 1 3\n8 and 1 2 7\n9 next 1 4 8\n";
    // Reset-artifact game `st' = rst ? 0 : c`: with `rst` adversarial the env holds reset → unrealizable;
    // `assume_clock_reset` pins the reset inactive → the functional game `st'=c` is realizable.
    const GAME_RESET: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 rst\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 ite 1 3 5 2\n8 next 1 4 7\n";
    // Saturating 1-bit buffer `count' = (c∧¬e) ∨ (count∧¬(e∧¬c))`: recurrence `GF(count==1)` (ctrl=c) is
    // unrealizable (env pops forever), but realizable under the fairness assumption `GF(e==0)`.
    const GAME_BUFFER: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e\n4 state 1 count\n5 zero 1\n6 init 1 4 5\n7 not 1 3\n8 and 1 2 7\n9 not 1 2\n10 and 1 3 9\n11 not 1 10\n12 and 1 4 11\n13 or 1 8 12\n14 next 1 4 13\n";
    // TWOGATE: two independent env gates (e1,e2) to `st==2`; recurrence needs BOTH `GF(e1==0)` and
    // `GF(e2==0)` — no single fairness suffices ⇒ the conjunctive fallback.
    const GAME_TWOGATE: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 c\n4 input 1 e1\n5 input 1 e2\n6 state 2 st\n7 zero 2\n8 init 2 6 7\n9 one 2\n10 constd 2 2\n11 not 1 4\n12 and 1 3 11\n13 not 1 5\n14 and 1 3 13\n15 eq 1 6 7\n16 eq 1 6 9\n17 ite 2 12 9 7\n18 ite 2 14 10 9\n19 ite 2 16 18 7\n20 ite 2 15 17 19\n21 next 2 6 20\n";

    /// `assume_clock_reset` over HTTP: the reset-adversarial game is unrealizable raw, but realizable when
    /// the reset is modeled as a posture (the modeling-artifact fix).
    #[tokio::test]
    async fn btor2_game_handler_assume_clock_reset_removes_reset_artifact() {
        let raw = Btor2GameRequest {
            content: GAME_RESET.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: false,
            objective: crate::api::models::GameObjective::Reach,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(raw)).await.expect("game runs");
        assert!(
            !out.realizable,
            "reset-adversarial ⇒ spuriously unrealizable"
        );
        let posture = Btor2GameRequest {
            content: GAME_RESET.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: false,
            objective: crate::api::models::GameObjective::Reach,
            assume_clock_reset: true,
        };
        let Json(out) = btor2_game_handler(Json(posture)).await.expect("game runs");
        assert!(
            out.realizable,
            "reset as posture ⇒ the controller wins st'=c"
        );
    }

    #[tokio::test]
    async fn btor2_game_handler_realizable_returns_controller_strategy() {
        use crate::adapter::btor2::symbolic_bitblast::TwoPlayerStrategy;
        let request = Btor2GameRequest {
            content: GAME_CTRL.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: false,
            objective: crate::api::models::GameObjective::Reach,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(request)).await.expect("game runs");
        assert!(out.realizable, "st'=c: the controller wins");
        assert!(matches!(
            out.strategy,
            Some(TwoPlayerStrategy::ControllerStrategy(_))
        ));
    }

    #[tokio::test]
    async fn btor2_game_handler_unrealizable_returns_counterstrategy() {
        use crate::adapter::btor2::symbolic_bitblast::TwoPlayerStrategy;
        let request = Btor2GameRequest {
            content: GAME_ENVBLK.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: false,
            objective: crate::api::models::GameObjective::Reach,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(request)).await.expect("game runs");
        assert!(!out.realizable, "st'=c&¬e: the environment wins");
        assert!(matches!(
            out.strategy,
            Some(TwoPlayerStrategy::EnvironmentCounterstrategy(_))
        ));
    }

    /// `--discover-assumptions` over HTTP: the unrealizable ENVBLK game returns a `holds_under` carrying
    /// the enabling environment assumption `e == 0` (CONDITIONAL — `realizable` stays false).
    #[tokio::test]
    async fn btor2_game_handler_discovers_env_assumption() {
        let request = Btor2GameRequest {
            content: GAME_ENVBLK.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: true,
            objective: crate::api::models::GameObjective::Reach,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(request)).await.expect("game runs");
        assert!(
            !out.realizable,
            "canonical realizable stays false (conditional-only)"
        );
        assert!(
            out.holds_under
                .iter()
                .any(|a| a.phi == "e == 0" && a.non_vacuous),
            "holds_under carries the enabling assumption e==0: {:?}",
            out.holds_under
        );
    }

    /// `objective: "recurrence"` over HTTP: the Büchi game on `st'=c` is realizable (the controller keeps
    /// `st==1` forever), the objective is echoed, and the CONTROLLER's Büchi strategy is returned (a
    /// `controller_strategy` over the `st` register — force `good` infinitely often).
    #[tokio::test]
    async fn btor2_game_handler_recurrence_realizable_emits_controller_strategy() {
        use crate::adapter::btor2::symbolic_bitblast::TwoPlayerStrategy;
        let request = Btor2GameRequest {
            content: GAME_CTRL.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: false,
            objective: crate::api::models::GameObjective::Recurrence,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(request)).await.expect("game runs");
        assert!(out.realizable, "st'=c: the controller forces st=1 i.o.");
        assert_eq!(out.objective, crate::api::models::GameObjective::Recurrence);
        match out.strategy {
            Some(TwoPlayerStrategy::ControllerStrategy(m)) => {
                assert_eq!(
                    m.state_register, "st",
                    "strategy indexed by the state register"
                );
            }
            other => {
                panic!("realizable recurrence must emit a controller Büchi strategy, got {other:?}")
            }
        }
    }

    /// `objective: "recurrence"` + `discover_assumptions` over HTTP: the unrealizable buffer recurrence
    /// game returns a `holds_under` carrying the FAIRNESS assumption `GF(e == 0)` (CONDITIONAL —
    /// `realizable` stays false).
    #[tokio::test]
    async fn btor2_game_handler_discovers_fairness_assumption() {
        let request = Btor2GameRequest {
            content: GAME_BUFFER.to_string(),
            good: "count == 1".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: true,
            objective: crate::api::models::GameObjective::Recurrence,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(request)).await.expect("game runs");
        assert!(
            !out.realizable,
            "recurrence is unrealizable without a fairness assumption"
        );
        assert!(
            out.holds_under.iter().any(|a| a.phi == "GF(e == 0)"
                && a.kind == crate::verdict::AssumptionKind::InputFairness
                && a.non_vacuous),
            "holds_under carries the fairness assumption GF(e==0): {:?}",
            out.holds_under
        );
    }

    /// `objective: "recurrence"` + `discover_assumptions` over HTTP on TWOGATE: no single fairness
    /// suffices, so the discovery falls through to a CONJUNCTION `GF(e1==0) && GF(e2==0)`
    /// (`InputFairnessConjunction`).
    #[tokio::test]
    async fn btor2_game_handler_discovers_fairness_conjunction() {
        let request = Btor2GameRequest {
            content: GAME_TWOGATE.to_string(),
            good: "st == 2".to_string(),
            controllable: vec!["c".to_string()],
            discover_assumptions: true,
            objective: crate::api::models::GameObjective::Recurrence,
            assume_clock_reset: false,
        };
        let Json(out) = btor2_game_handler(Json(request)).await.expect("game runs");
        assert!(!out.realizable, "no single fairness rescues TWOGATE");
        assert!(
            out.holds_under.iter().any(|a| a.phi.contains("GF(e1 == 0)")
                && a.phi.contains("GF(e2 == 0)")
                && a.kind == crate::verdict::AssumptionKind::InputFairnessConjunction),
            "holds_under carries the fairness conjunction: {:?}",
            out.holds_under
        );
    }

    #[tokio::test]
    async fn btor2_game_handler_rejects_unknown_controllable_input() {
        let request = Btor2GameRequest {
            content: GAME_CTRL.to_string(),
            good: "st == 1".to_string(),
            controllable: vec!["nope".to_string()], // not a primary input
            discover_assumptions: false,
            objective: crate::api::models::GameObjective::Reach,
            assume_clock_reset: false,
        };
        let err = btor2_game_handler(Json(request))
            .await
            .expect_err("an unknown controllable input must be rejected");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    // SV-direct verbs validate the atoms BEFORE the (toolchain-gated) SV→BTOR2 lift,
    // so the guard is a BadRequest reachable in make-ci without sv2v / Yosys. The full
    // lift → verdict path is covered by the mununu-sva e2e suite.
    #[tokio::test]
    async fn sv_verify_liveness_handler_rejects_malformed_atom_before_lift() {
        let request = SvVerifyLivenessRequest {
            source: "module m; endmodule".to_string(),
            additional_sources: vec![],
            top: None,
            use_sv2v: false,
            use_slang: false,
            request: "x == y".to_string(), // relational — rejected before the lift
            grant: "st == 1".to_string(),
        };
        let err = sv_verify_liveness_handler(Json(request))
            .await
            .expect_err("relational request atom must be rejected pre-lift");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }

    #[tokio::test]
    async fn sv_verify_recoverability_handler_rejects_malformed_target_before_lift() {
        let request = SvVerifyRecoverabilityRequest {
            source: "module m; endmodule".to_string(),
            additional_sources: vec![],
            top: None,
            use_sv2v: false,
            use_slang: false,
            target: "not an atom !!".to_string(),
            predicates: Vec::new(),
            refine: false,
            config_values: Vec::new(),
            discover_assumptions: false,
        };
        let err = sv_verify_recoverability_handler(Json(request))
            .await
            .expect_err("malformed target must be rejected pre-lift");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }
}
