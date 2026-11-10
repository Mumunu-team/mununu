//! The verify-auto entry points (mununu#964): the run, the plan-only report and the mutation
//! comparison. They decide which path a run takes — the planner's portfolio pipeline or the
//! single picked engine — so they live in the planner, which depends on the engine layer
//! (`adapter::slang::verify_auto`) and never the other way round. `verify_auto_impl`, the
//! per-engine leaf, stays in the engine layer.

#![allow(clippy::wildcard_imports)]

use crate::adapter::slang::verify_auto::*;
use crate::adapter::yosys::YosysOptions;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// One property's baseline-vs-mutant verdict under a mutation (`sv mutate`).
#[derive(Debug, Clone)]
pub struct PropertyFlip {
    /// The property (assertion) name — the join key between baseline and mutant.
    pub name: String,
    /// The SVA label (`a_trip_latches: assert property …`), when the source gives one.
    pub label: Option<String>,
    /// The canonical verdict on the design as-lifted.
    pub baseline: String,
    /// The canonical verdict on the mutated design.
    pub mutant: String,
    /// `baseline != mutant` — the mutation was *caught* by this property.
    pub flipped: bool,
}

/// The result of an `sv mutate` run: whether a NAMED structural mutation flips the
/// property verdicts. A **flip** confirms the property constrains the mutated
/// behaviour; an **unflipped** property is the property-adequacy finding — the
/// spec does not pin that fault down (vacuous w.r.t. the mutation). Per
/// [claims-integrity §2] this is a statement about the PROPERTIES, never a bug
/// report about the design.
#[derive(Debug, Clone)]
pub struct MutateReport {
    /// The applied mutation's label (`stick:<reg>` / `drop-reset:<reg>`).
    pub mutation: String,
    /// Per-property baseline-vs-mutant comparison (baseline order).
    pub properties: Vec<PropertyFlip>,
    /// Number of properties the mutation flipped (caught it).
    pub flipped: usize,
    /// Number the mutation did NOT flip (the adequacy finding).
    pub unflipped: usize,
}

impl MutateReport {
    /// The mutation run's verdict: `holds` when at least one property caught the mutant (a flip),
    /// `violated` when it survived every property — the spec gap a CI gate fails on.
    pub fn outcome(&self) -> &'static str {
        if self.flipped > 0 {
            "holds"
        } else {
            "violated"
        }
    }
}

/// `sv mutate` core — verify the design **twice** (as-lifted baseline, then with
/// `mutation` applied in `prepare_model`) and diff the per-property verdicts by
/// name. A mutation that names a missing register / does not apply is a hard error
/// from the mutant lift (never a silent no-op). `base_opts.mutation` is ignored
/// (forced `None` for the baseline, `Some(mutation)` for the mutant).
pub fn mutate_and_compare(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    base_opts: &VerifyAutoOptions,
    mutation: crate::adapter::btor2::mutate::Mutation,
) -> Result<MutateReport, AdapterError> {
    let mut baseline_opts = base_opts.clone();
    baseline_opts.mutation = None;
    let baseline = verify_auto(sources, yosys_opts, &baseline_opts)?;

    let mut mutant_opts = base_opts.clone();
    mutant_opts.mutation = Some(mutation.clone());
    let mutant = verify_auto(sources, yosys_opts, &mutant_opts)?;

    let mutant_by_name: std::collections::HashMap<&str, &VerifyOutcome> = mutant
        .properties
        .iter()
        .map(|p| (p.name.as_str(), &p.outcome))
        .collect();

    let properties: Vec<PropertyFlip> = baseline
        .properties
        .iter()
        .map(|b| {
            let baseline = b.outcome.label();
            // A property present at baseline but absent from the mutant reads as
            // `skipped` (the mutation dropped it) — still a flip if baseline was definite.
            let mutant = mutant_by_name
                .get(b.name.as_str())
                .map_or("skipped", |o| o.label());
            PropertyFlip {
                name: b.name.clone(),
                label: b.label.clone(),
                baseline: baseline.to_string(),
                mutant: mutant.to_string(),
                flipped: baseline != mutant,
            }
        })
        .collect();

    let flipped = properties.iter().filter(|p| p.flipped).count();
    let unflipped = properties.len() - flipped;
    Ok(MutateReport {
        mutation: mutation.as_label(),
        properties,
        flipped,
        unflipped,
    })
}

pub fn verify_auto(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<AutoVerifyReport, AdapterError> {
    // N0.1 — `MUNUNU_VERIFY_AUTO_ONLY_ROWS` narrows the run to the named rows (the executable
    // ledger's re-run of what changed); the row facts are written once the report is final,
    // whichever path produced it.
    let mut opts = crate::adapter::env_var::read("MUNUNU_VERIFY_AUTO_ONLY_ROWS")
        .and_then(|v| parse_only_rows(&v))
        .map(|rows| VerifyAutoOptions {
            only_rows: Some(rows),
            ..opts.clone()
        })
        .unwrap_or_else(|| opts.clone());
    // mununu#933 — the run's input identity, on every breadcrumb record and checked by a resume.
    opts.run_fingerprint = Some(crate::adapter::resume::run_fingerprint(
        sources, yosys_opts, &opts,
    ));
    // J4c — the run's own wall, measured where the run begins and ends.
    let run_t0 = std::time::Instant::now();
    let mut report = verify_auto_with(sources, yosys_opts, &opts)?;
    crate::adapter::row_facts::write_from_env(&report);
    report.run_ms = Some(u64::try_from(run_t0.elapsed().as_millis()).unwrap_or(u64::MAX));
    Ok(report)
}

/// [`verify_auto`] after the environment's row selection — roadmap J3's resume wraps the
/// dispatch: the breadcrumb is read and checked BEFORE any lift (a refusal costs nothing), the
/// rows it decided join `skip_rows` (every member of a portfolio honours the set, see
/// [`crate::planner::member_options`]), and their verdicts are overlaid on the final report.
fn verify_auto_with(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<AutoVerifyReport, AdapterError> {
    let Some(text) = opts.resume_from.as_deref() else {
        return verify_auto_dispatch(sources, yosys_opts, opts);
    };
    let current = crate::build_info::current().map(crate::build_info::build_tag);
    let plan = crate::adapter::resume::parse_breadcrumb(
        text,
        current.as_deref(),
        opts.run_fingerprint.as_deref().unwrap_or_default(),
    )
    .map_err(|refusal| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("resume-from: {refusal}"),
        location: None,
    })?;
    let mut resumed_opts = opts.clone();
    resumed_opts.skip_rows.extend(plan.resumed.keys().cloned());
    let mut report = verify_auto_dispatch(sources, yosys_opts, &resumed_opts)?;
    apply_resume(&mut report, &plan);
    record_resumed_breadcrumb(&report, opts.run_fingerprint.as_deref());
    Ok(report)
}

/// Roadmap J2 — the plan of a run, without the run: the lift and the planner's per-property routing
/// with its cost estimate, and no engine. `sv verify-auto --plan-only` and `POST /api/v1/sv/plan`.
#[derive(Debug, Clone, Default)]
pub struct PlanReport {
    /// Every property the run would verify, `(name, label, formula)` in report order.
    pub properties: Vec<(String, Option<String>, String)>,
    /// The planner's rationale (with [`crate::routing::RoutingRationale::estimate`]) per property
    /// whose cone width could be established; a property without one has no estimate (mununu#577).
    pub routing: Vec<crate::routing::RoutingRationale>,
    /// mununu#941 — the properties with no estimate, each with its reason.
    pub misses: Vec<crate::routing::RoutingMiss>,
    /// mununu#939 — the properties the run would NOT execute, each with why: resumed from the
    /// breadcrumb, or left out by the row selection (`MUNUNU_VERIFY_AUTO_ONLY_ROWS`, `skip_rows`).
    /// They are still listed (the run reports them, as not attempted) but carry no band.
    pub not_run: Vec<(String, String)>,
    /// Assertions that did not translate, as in the run's report.
    pub unsupported: Vec<(String, String)>,
    /// The lift's wall, milliseconds — paid once per run, not per property.
    pub lift_ms: u64,
}

/// [`PlanReport`] for these inputs: `prepare_model` (the same lift the run does) and
/// [`crate::planner::plan`]'s rationale. Never runs an engine, so it costs the lift alone.
pub fn plan_auto(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<PlanReport, AdapterError> {
    // mununu#939 — the run's row selection, applied the way the run applies it: the
    // `MUNUNU_VERIFY_AUTO_ONLY_ROWS` selection (by name or label), `skip_rows`, and the rows a
    // `resume_from` breadcrumb decided — checked BEFORE the lift against this run's input
    // identity, so a breadcrumb the run would refuse fails the plan the same way.
    let only_rows = crate::adapter::env_var::read("MUNUNU_VERIFY_AUTO_ONLY_ROWS")
        .and_then(|v| parse_only_rows(&v))
        .or_else(|| opts.only_rows.clone());
    let resumed = match opts.resume_from.as_deref() {
        None => std::collections::BTreeMap::new(),
        Some(text) => {
            let current = crate::build_info::current().map(crate::build_info::build_tag);
            let run = crate::adapter::resume::run_fingerprint(sources, yosys_opts, opts);
            crate::adapter::resume::parse_breadcrumb(text, current.as_deref(), &run)
                .map_err(|refusal| AdapterError {
                    kind: AdapterErrorKind::ParseError,
                    message: format!("resume-from: {refusal}"),
                    location: None,
                })?
                .resumed
        }
    };
    let not_run_reason = |name: &str, label: Option<&String>| {
        row_not_run_reason(name, label, only_rows.as_ref(), &opts.skip_rows, &resumed)
    };
    let t0 = std::time::Instant::now();
    let prepared = prepare_model(sources, yosys_opts, opts)?;
    let lift_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(match prepared {
        Prepared::Empty(report) => PlanReport {
            unsupported: report.unsupported,
            lift_ms,
            ..PlanReport::default()
        },
        Prepared::Model(m) => {
            let plan = crate::planner::plan(Some(&m), opts);
            // The assumption-consistency check is a row the run folds into
            // `report.assumptions.consistency` and removes (`finish_assumptions`), so the plan
            // does not list it either — the plan names exactly the run's rows (the box's
            // out-of-sample check found 16 planned rows against 15 reported on two designs).
            let reported = |name: &str| name != ASSUMPTION_CONSISTENCY_PROPERTY;
            PlanReport {
                properties: m
                    .extraction
                    .translated
                    .iter()
                    .filter(|t| reported(&t.name))
                    .map(|t| (t.name.clone(), t.label.clone(), t.formula.clone()))
                    .collect(),
                routing: plan
                    .rationale
                    .into_iter()
                    .filter(|r| reported(&r.property))
                    .collect(),
                misses: plan
                    .misses
                    .into_iter()
                    .filter(|r| reported(&r.property))
                    .collect(),
                not_run: m
                    .extraction
                    .translated
                    .iter()
                    .filter(|t| reported(&t.name))
                    .filter_map(|t| {
                        not_run_reason(&t.name, t.label.as_ref()).map(|why| (t.name.clone(), why))
                    })
                    .collect(),
                unsupported: m.report.unsupported.clone(),
                lift_ms,
            }
        }
    })
}

/// The dispatch as it was before J3: portfolio through the planner, else the single engine.
fn verify_auto_dispatch(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<AutoVerifyReport, AdapterError> {
    // PORTFOLIO dispatch — when a portfolio mode is set, run the common-IR-hub pipeline:
    // `prepare_model` once → `plan()` (the ladder + the per-property cost RATIONALE, computed from
    // the shared prepared model) → `execute()` (drive each operator over the shared model, merge
    // under the runtime soundness guard, attach the rationale telemetry). Verdict-equivalent to the
    // former inline `verify_auto_portfolio`; the routing reasoning now lives in the planner.
    if opts.portfolio.is_some() {
        // J4c — the lift is paid once per run, here; the members share the prepared model.
        let lift_t0 = std::time::Instant::now();
        return match prepare_model(sources, yosys_opts, opts)? {
            Prepared::Empty(report) => {
                let mut report = *report;
                report.lift_ms =
                    Some(u64::try_from(lift_t0.elapsed().as_millis()).unwrap_or(u64::MAX));
                finish_assumptions(&mut report);
                Ok(report)
            }
            Prepared::Model(m) => {
                let lift_ms = u64::try_from(lift_t0.elapsed().as_millis()).unwrap_or(u64::MAX);
                let m = *m;
                let task = crate::planner::VerificationTask {
                    sources,
                    yosys_opts,
                    opts,
                };
                let plan = crate::planner::plan(Some(&m), opts);
                let mut report = crate::planner::execute(&plan, &m, &task)?;
                // The merge builds its report from the operators'; the assumption summary is the
                // prepared model's, whatever the merge kept.
                report.assumptions = m.report.assumptions.clone();
                // …and so is the lift's wall (the members lifted nothing).
                report.lift_ms = Some(lift_ms);
                finish_assumptions(&mut report);
                Ok(report)
            }
        };
    }

    // Single-engine path (a user-picked `--engine`): the direct `verify_auto_impl(None)` form —
    // it checks A.6 (exact + no-reset-gating rejection) at the top, BEFORE any lift, then builds
    // the model inline and runs the one engine. No cost rationale — the predicted engine is
    // trivially the one the user selected. (Only the portfolio uses the plan/execute pipeline.)
    let mut report = verify_auto_impl(sources, yosys_opts, opts, None)?;
    finish_assumptions(&mut report);
    Ok(report)
}
