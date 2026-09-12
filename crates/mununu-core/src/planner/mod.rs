//! Verification execution planner (verification-execution-planner Phase 3 / roadmap P2.1–P2.2).
//!
//! The portfolio `verify-auto` path runs a four-stage pipeline (the common-IR hub):
//!
//! 1. [`prepare_model`](crate::adapter::slang::verify_auto::prepare_model) — build the shared
//!    prepared model ONCE (slang extract + yosys lift + reset/pin), an operator-independent IR.
//! 2. [`plan`] — the ladder (exact → cube → explicit) **plus** the per-property COST RATIONALE:
//!    from the model's facts (`property_class` × cone-vs-cap × diameter proxy) predict which engine
//!    will decide each property, and why (the decision table). This is where the planner's
//!    reasoning lives.
//! 3. [`execute`] — drive each operator over the shared model, merge under the runtime soundness
//!    guard ([`merge_portfolio_reports`](crate::adapter::slang::verify_auto::merge_portfolio_reports)),
//!    and attach the rationale as `plan-cost` / `plan-accuracy` telemetry ONCE (never a verdict change).
//! 4. `verify_auto_impl` — the pure per-engine leaf; runs one engine, no planning.
//!
//! **Verdict-equivalent by construction:** the plan is a fixed precision ladder + reactive rescue
//! (`escalate_bottom`) + early-exit; the rationale is a telemetry hint, not a dispatch decision (a
//! sound faster-abstain on the diameter case is un-achievable — see the plan doc). The single-engine
//! path (a user-picked `--engine`) skips plan/execute and runs `verify_auto_impl` directly.
//!
//! See `.claude/plans/verification-execution-planner.md` + `.claude/plans/cost-annotated-plan-telemetry.md`.

use crate::adapter::slang::verify_auto::{
    AutoVerifyReport, NoteLevel, PortfolioMode, PreparedModel, VerificationNote, VerifyAutoOptions,
    VerifyOutcome, escalate_bottom, merge_portfolio_reports, outcome_definite,
    rescue_skipped_via_exact, verify_auto_impl,
};
use crate::adapter::yosys::YosysOptions;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// One physical engine operator — a single-engine [`verify_auto`] pass.
///
/// P2.1a covers the μ-calculus engine leaves (the precision ladder). Abstraction transforms
/// (cutpoint / predicate-seed) and the ⊥-reactive rescue edges become operators in later
/// increments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOp {
    /// Mirrors the CLI/API `--engine` label (the value the merge keys provenance on).
    pub label: &'static str,
    /// Select the may/must cube ("symbolic") engine.
    pub symbolic_engine: bool,
    /// Select the full-state exact BDD ("exact-symbolic") engine.
    pub exact_symbolic: bool,
}

/// The physical plan — the ordered engine operators, how to schedule them, and the per-property
/// COST RATIONALE (which engine the planner predicts will decide each property, and why). The
/// rationale is the planner's reasoning made explicit; it is attached as `plan-cost` /
/// `plan-accuracy` telemetry by [`execute`], never a verdict change.
#[derive(Debug, Clone)]
pub struct PhysicalPlan {
    /// Operators in precision order (exact first).
    pub ops: Vec<EngineOp>,
    /// `Sequential` = run in order, merge after each, stop when no ⊥ property remains (the
    /// budget win); `Parallel` = run all concurrently, merge once. A single-op plan runs the
    /// same under either mode.
    pub mode: PortfolioMode,
    /// Per-property routing rationale (empty when the plan was built without a prepared model —
    /// e.g. the plan unit-tests). Computed by [`plan`] from the prepared model's facts.
    pub rationale: Vec<RoutingRationale>,
}

/// The logical verification task — the planner's input.
///
/// **P2.1a form:** the *pre-lift bundle* the SV `verify-auto` entry already holds (sources +
/// lift options + verify options). It migrates to the plan doc's
/// `(Sts, MuFormula, posture, ModelFacts, hints, budget)` form when the common-IR hub lands
/// (Phase 2); the seam is introduced now so later phases extend this struct, not the call
/// graph. Borrows its inputs — cheap to build at the dispatch point.
pub struct VerificationTask<'a> {
    /// `(file_name, content)`, the first being the primary source.
    pub sources: &'a [(String, String)],
    /// The SV → BTOR2 lift options.
    pub yosys_opts: &'a YosysOptions,
    /// The verify-auto options (engine intent, budgets, rescue gates, hints).
    pub opts: &'a VerifyAutoOptions,
}

/// The μ-calculus engine precision ladder (exact → symbolic-cube → explicit-CEGAR) — the
/// order the portfolio has always used. Exact first: 2-valued, never-⊥ within its bit cap,
/// richest witness; the two cube engines are complementary fallbacks the parity differential
/// proved never contradict it. (Relocated verbatim from `verify_auto`'s `PORTFOLIO_ENGINES`.)
const PRECISION_LADDER: [EngineOp; 3] = [
    EngineOp {
        label: "exact-symbolic",
        symbolic_engine: false,
        exact_symbolic: true,
    },
    EngineOp {
        label: "symbolic",
        symbolic_engine: true,
        exact_symbolic: false,
    },
    EngineOp {
        label: "explicit",
        symbolic_engine: false,
        exact_symbolic: false,
    },
];

/// Rule-based planner (P2.1a): reproduce today's fixed engine selection. A portfolio intent
/// emits the full precision ladder in the requested mode; a single-engine intent emits just
/// that operator. (P2.2 makes this cost-based off `ModelFacts` — cone-vs-cap, property-class
/// → verdict requirement.)
pub(crate) fn plan(prepared: Option<&PreparedModel>, opts: &VerifyAutoOptions) -> PhysicalPlan {
    let (ops, mode) = match opts.portfolio {
        Some(mode) => (PRECISION_LADDER.to_vec(), mode),
        None => (
            vec![EngineOp {
                label: single_engine_label(opts),
                symbolic_engine: opts.symbolic_engine,
                exact_symbolic: opts.exact_symbolic,
            }],
            PortfolioMode::Sequential,
        ),
    };
    // The planner's cost reasoning, made explicit: from the prepared model's facts, predict the
    // deciding engine + why for each property (the decision table). Empty when no model is supplied
    // (the plan unit-tests build the ladder alone). This is what `execute` attaches as telemetry.
    let rationale = prepared
        .map(|m| annotate_routing(&m.btor2, &m.properties()))
        .unwrap_or_default();
    PhysicalPlan {
        ops,
        mode,
        rationale,
    }
}

/// The `--engine` label a single-engine option pair resolves to (exact takes precedence, as
/// in `engine_selection`).
fn single_engine_label(opts: &VerifyAutoOptions) -> &'static str {
    match (opts.symbolic_engine, opts.exact_symbolic) {
        (_, true) => "exact-symbolic",
        (true, false) => "symbolic",
        (false, false) => "explicit",
    }
}

/// Execute the plan: run each engine operator as a single-engine [`verify_auto`] pass and
/// merge under the runtime soundness guard. Relocated verbatim from the former
/// `verify_auto_portfolio` — behaviour-identical, so the portfolio's verdicts are unchanged.
pub(crate) fn execute(
    plan: &PhysicalPlan,
    prepared: &PreparedModel,
    task: &VerificationTask,
) -> Result<AutoVerifyReport, AdapterError> {
    // A single-engine option set derived from `task.opts` with the portfolio disabled and the
    // two engine flags forced to this operator's pair (so the inner call runs exactly one
    // engine and does not recurse).
    let mk_opts = |op: &EngineOp| {
        let mut o = task.opts.clone();
        o.portfolio = None;
        o.symbolic_engine = op.symbolic_engine;
        o.exact_symbolic = op.exact_symbolic;
        o
    };

    // Attach the plan's cost RATIONALE as telemetry on the final report — one `plan-cost` note per
    // property + one `plan-accuracy` self-check. The precision ladder + reactive rescue decided the
    // verdicts; this only explains + checks the routing (never a verdict change). Computed ONCE by
    // `plan()` from the shared prepared model, so it is not recomputed per operator (the reasoning
    // lives in the planner now, not in `verify_auto_impl`).
    let attach = |mut rep: AutoVerifyReport| -> AutoVerifyReport {
        for r in &plan.rationale {
            rep.notes.push(routing_rationale_note(r));
        }
        if let Some(a) = plan_accuracy_note(&plan.rationale, &rep) {
            rep.notes.push(a);
        }
        rep
    };

    match plan.mode {
        PortfolioMode::Sequential => {
            let mut runs: Vec<(&str, Result<AutoVerifyReport, AdapterError>)> = Vec::new();
            for op in &plan.ops {
                runs.push((
                    op.label,
                    verify_auto_impl(
                        task.sources,
                        task.yosys_opts,
                        &mk_opts(op),
                        Some(prepared.clone()),
                    ),
                ));
                // Early-exit as soon as the MERGE so far leaves no ⊥ property (the budget win).
                let merged = merge_portfolio_reports(&runs, plan.mode);
                if let Ok(rep) = &merged
                    && rep
                        .properties
                        .iter()
                        .all(|p| outcome_definite(&p.outcome).is_some())
                {
                    return merged.map(&attach);
                }
            }
            merge_portfolio_reports(&runs, plan.mode).map(&attach)
        }
        PortfolioMode::Parallel => {
            // Scoped threads borrow the task's sources / yosys_opts; each owns its option clone
            // AND its own clone of the shared prepared model (so no engine re-runs the prep).
            // mununu#504 — a spawned thread inherits NOTHING from the parent's thread-local
            // budget, so each engine thread must re-install it, or the parallel portfolio would
            // silently run unbounded while the sequential one is bounded.
            // `run_budget`'s `budget_is_not_inherited_by_a_spawned_thread` pins that behaviour,
            // and this is the site it exists to protect.
            let parent_budget = crate::adapter::run_budget::current();
            let runs: Vec<(&str, Result<AutoVerifyReport, AdapterError>)> =
                std::thread::scope(|scope| {
                    let handles: Vec<(&str, _)> = plan
                        .ops
                        .iter()
                        .map(|op| {
                            let o = mk_opts(op);
                            let pm = prepared.clone();
                            let b = parent_budget.clone();
                            (
                                op.label,
                                scope.spawn(move || {
                                    let _g = crate::adapter::run_budget::enter(&b);
                                    verify_auto_impl(task.sources, task.yosys_opts, &o, Some(pm))
                                }),
                            )
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|(label, h)| {
                            let r = h.join().unwrap_or_else(|_| {
                                Err(AdapterError {
                                    kind: AdapterErrorKind::IrConsistencyError,
                                    message: format!("portfolio: engine `{label}` panicked"),
                                    location: None,
                                })
                            });
                            (label, r)
                        })
                        .collect()
                });
            merge_portfolio_reports(&runs, plan.mode).map(&attach)
        }
    }
}

/// The planner's reactive **re-plan** step (verification-execution-planner §4.5 step 4 /
/// roadmap P2.1b) — when the main-path plan ([`execute`]) leaves a property `⊥`, feed the
/// reason back and route it to a sound reduction. The re-plan **edges**, dispatched by
/// property class (each gated by its own `rescue_bottom_*` opt, firing only on its shape):
///
/// - **safety** `AG`-invariant → the reachability portfolio (reduce to a `bad`-monitor);
/// - **box-AF response** `AG(a→AF b)` → liveness-to-safety (l2s) → the portfolio;
/// - **νμ recoverability** `AG EF good` → exact+COI (reset-pinned) or cube+`smt-hyper-must`.
///
/// P2.1b routes the re-plan **entry** through the planner (so both halves of the
/// orchestration — main-path [`plan`]/[`execute`] and this reactive step — are planner-owned)
/// while **delegating the edge application** to the existing rescue subsystem
/// ([`escalate_bottom`]). Verdict-equivalent. P2.1c adds the soundness plan-invariants here
/// (cube-νμ exact-corroboration; property-class-aware transfer reporting).
pub fn replan(
    report: &mut AutoVerifyReport,
    design_btor2: &str,
    reset_pinned: bool,
    opts: &VerifyAutoOptions,
    fairness_atoms: &[crate::adapter::liveness_rescue::Atom],
) -> Vec<VerificationNote> {
    escalate_bottom(report, design_btor2, reset_pinned, opts, fairness_atoms)
}

/// A **composed** recoverability plan — the point of a *planner*: mechanisms combine. It runs the
/// SOUND, transferable levers as one plan on the (caller-scoped) model:
///
///   exact-first (COI-shrunk) → cube + ranking + guard-atoms + **Craig** on the same model.
///
/// Every verdict it returns TRANSFERS to the design as given: a definite exact verdict, or a
/// definite 3-valued-KMTS verdict (sound at every alternation depth incl. νμ, Bruns–Godefroid).
///
/// **No auto-input-pinning (removed 2026-07-26 — it was unsound).** An earlier version config-pinned
/// the in-cone free inputs to 0 as a "combination component" and returned the pinned verdict. That is
/// UNSOUND for `AG EF`: recoverability is **not monotone under input-restriction**. Pinning an input
/// to a constant restricts BOTH the reachable states (weakening the outer `AG`) AND the available
/// recovery paths (strengthening the inner `EF`), so neither direction transfers —
/// - a pinned **HOLDS** omits every other input valuation (holds only for the pins), and
/// - a pinned **VIOLATED** can be spurious, because pinning removes the very recovery transitions the
///   inputs would steer through.
///
/// Measured on the wall-class set: the only ⊥→"decided" case the input-pin produced was `staller` —
/// a genuinely VIOLATED design whose stall the input-pin *hid*, yielding a non-transferable scoped
/// HOLDS that (had the scope been dropped) is a spurious verdict. Auto-config-value was already 0
/// full-space payoff. A caller wanting an operational sub-question pins RESET itself and passes the
/// scoped model in — reset-gating is sound and is the caller's scoping to make, not this plan's to
/// apply silently to free inputs.
pub fn solve_recoverability_combined(
    design_btor2: &str,
    target: &str,
) -> Result<crate::verdict::PropertyVerdict, String> {
    use crate::adapter::btor2::cegar::PredicateSource;
    use crate::adapter::recoverability::{
        verify_recoverability, verify_recoverability_scalable_with_source,
    };
    use crate::verdict::PropertyVerdict;

    // exact-first (with COI) — the strongest sound oracle; then the scalable cube path, which itself
    // composes cube + ranking + guard-atoms + Craig. Both transfer to the model as given.
    let exact_first = verify_recoverability(design_btor2, target);
    if matches!(
        exact_first,
        Ok(PropertyVerdict::Holds | PropertyVerdict::Violated)
    ) {
        return exact_first;
    }
    verify_recoverability_scalable_with_source(
        design_btor2,
        target,
        &[],
        PredicateSource::CraigInterpolation,
    )
}

/// The companion re-plan edge for cube-**Skipped** (not `⊥`) properties: an atom-less modal
/// formula the cube cannot seed gets one full-state exact-symbolic attempt, upgraded on a
/// definite verdict (lever b). Gated to the cube path under reset-gating. Delegates to the
/// existing [`rescue_skipped_via_exact`] — verdict-equivalent.
pub fn rescue_skipped(
    report: &mut AutoVerifyReport,
    design_btor2: &str,
    opts: &VerifyAutoOptions,
) -> Vec<VerificationNote> {
    let mut notes = rescue_skipped_via_exact(report, design_btor2, opts);
    // P2.2a — the first ModelFacts consumer: an actionable diagnostic for what remains
    // Skipped. Verdict-equivalent (a note, never a verdict change); P2.2b will *act* on the
    // same cone-vs-cap facts to auto-pin and re-decide.
    notes.extend(skip_over_cap_notes(report, design_btor2));
    notes
}

/// P2.2a/b — **residual-register-aware** actionable Skip diagnostic (the `ModelFacts`
/// consumer). For each property STILL Skipped after the exact attempt, use `ModelFacts` to
/// check whether the exact engine bailed because the property's cone exceeds the bit cap
/// (`cone_bits > cap`); if so, attach a note naming the cone width and the cap.
///
/// The **key fact** (P2.2b, measure-first 2026-07-26) is the *residual* — the register bits
/// that remain even after pinning **every** in-cone free input
/// (`residual = cone_bits − Σ pinnable_input_widths`). This is what decides whether
/// `--config-value` pinning can actually help:
///
/// - `residual ≤ cap` — **input-inflated** cone: pinning the in-cone inputs drops the cone to
///   ~`residual` bits ≤ cap, so `--config-value SIGNAL=VALUE` (a scoped verdict) decides it.
///   The note lists the pinnable inputs and how far pinning gets.
/// - `residual > cap` — **register-dominated** cone: even pinning ALL in-cone inputs leaves
///   `residual` register bits over the cap, so `--config-value` **cannot** decide it. The note
///   says so and points at the real levers (structural compression: one-hot re-encode /
///   counter abstraction; or cutpoint, posture-permitting).
///
/// A measure-first sweep (i2c 122 / gost 360 / present_cipher 144 residual register bits, all
/// above the 64-bit cap) showed the corpus's over-cap cones are register-dominated — so the
/// earlier "just pin the wide inputs" advice over-promised. This diagnostic tells the truth
/// per property instead of assuming input-inflation. Verdict-equivalent — a diagnostic, never
/// a verdict change.
fn skip_over_cap_notes(report: &AutoVerifyReport, design_btor2: &str) -> Vec<VerificationNote> {
    use crate::adapter::btor2::model_facts::ModelFacts;
    use crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms;

    let Ok(file) = crate::adapter::btor2::parser::parse(design_btor2) else {
        return Vec::new();
    };
    let facts = ModelFacts::new(&file);
    let mut notes = Vec::new();
    for prop in &report.properties {
        let VerifyOutcome::Skipped { reason } = &prop.outcome else {
            continue;
        };
        // mununu#462 — a bit-blast that ABSTAINED (BDD arena exhausted) mid-evaluation, rather
        // than at admission: name the cone's state-bit width and the width lever (`--param`).
        // This is the state-bit-count analogue of the skip-diameter-bound note. Matches both the
        // cube-path oom() string and the exact-build string (both contain "arena exhausted").
        if reason.contains("arena exhausted") {
            let cone_bits = crate::mu_calculus::parser::parse(&prop.formula)
                .map(|f| facts.cone_vs_cap(&formula_seed_atoms(&f)).0)
                .unwrap_or(0);
            notes.push(bitblast_oom_skip_note(&prop.name, cone_bits));
            continue;
        }
        let Ok(formula) = crate::mu_calculus::parser::parse(&prop.formula) else {
            continue;
        };
        let atoms = formula_seed_atoms(&formula);
        let (cone_bits, cap) = facts.cone_vs_cap(&atoms);
        if cone_bits <= cap {
            // Not a bit-CAP issue (the cone fits). Consult the DIAMETER proxy (P2.2c): an in-cone
            // down-counter of width W means the fixpoint may need ~2^W iterations, so exact
            // abstains on the ITERATION budget rather than the bit cap — a different, and more
            // actionable, Skip reason than "cone too wide". Emit a diameter-bound note when the
            // proxy can tell (it covers the reload-at-threshold down-counter shape only — up-
            // counters and non-counter diameters are honestly out of its reach); otherwise leave
            // it to the existing Skip provenance (atom-less cube, unsupported op).
            if let Some(w) = facts.cone_counter_diameter_log2(&atoms) {
                notes.push(diameter_bound_skip_note(&prop.name, w));
            }
            continue;
        }
        let inputs = facts.pinnable_cone_inputs(&atoms);
        let input_bits: u32 = inputs.iter().map(|i| i.width).sum();
        // The register bits that remain even after pinning EVERY in-cone input — the true test
        // of whether config-value pinning can bring the cone under the cap.
        let residual = cone_bits.saturating_sub(input_bits);
        let items: Vec<String> = inputs
            .iter()
            .take(8)
            .map(|i| format!("{}({} bits)", i.name, i.width))
            .collect();
        let detail = if residual <= cap {
            // Input-inflated: pinning these inputs fits the cap → config-value decides.
            format!(
                "The exact engine bit-blasts a property's cone-of-influence; this cone exceeds \
                 the bit cap, so it bailed to Skipped. The cone is INPUT-inflated: pinning the \
                 {} in-cone free input(s) below ({input_bits} bits) to representative constants \
                 (`--config-value SIGNAL=VALUE`) drops the cone to ~{residual} register bits (≤ \
                 the {cap}-bit cap), so exact decides — a verdict scoped to the pinned \
                 configuration.",
                inputs.len()
            )
        } else {
            // Register-dominated: even pinning all inputs leaves > cap register bits.
            format!(
                "The exact engine bit-blasts a property's cone-of-influence; this cone exceeds \
                 the bit cap, so it bailed to Skipped. This cone is REGISTER-dominated: even \
                 pinning ALL {} in-cone free input(s) ({input_bits} bits) leaves ~{residual} \
                 register bits (still > the {cap}-bit cap), so `--config-value` CANNOT bring it \
                 under the cap. The decidable lever here is sound register reduction — \
                 structural compression (one-hot re-encode, counter abstraction) or a cutpoint \
                 (safety-posture only; unsound for νμ) — not input concretization.",
                inputs.len()
            )
        };
        notes.push(VerificationNote {
            kind: "skip-over-cap-reason".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "`{}`: Skipped — the exact cone is {cone_bits} register+input bits (> the \
                 {cap}-bit cap); {} even fully input-pinned ({residual} residual register \
                 bits {} {cap}).",
                prop.name,
                if residual <= cap {
                    "config-value decidable"
                } else {
                    "register-dominated"
                },
                if residual <= cap { "≤" } else { ">" },
            ),
            detail,
            items,
        });
    }
    notes
}

/// P2.2c — the diameter-proxy Skip note: a property Skipped with its cone UNDER the bit cap, whose
/// cone carries a `W`-bit down-counter (⟹ up to 2^W fixpoint iterations). It tells the user the
/// abstention was the ITERATION budget (the reachable DIAMETER), not the bit cap — the sound lever
/// here is a well-founded RANKING certificate (a `verify-recoverability` escalation / the
/// recoverability rescue), not register reduction. Advisory only (a note, never a verdict change).
fn diameter_bound_skip_note(name: &str, counter_log2: u32) -> VerificationNote {
    VerificationNote {
        kind: "skip-diameter-bound".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{name}`: Skipped — the cone fits the bit cap, but an in-cone {counter_log2}-bit \
             counter gives a ~2^{counter_log2} state-space DIAMETER, so the exact fixpoint \
             abstains on the iteration budget (not the bit cap)."
        ),
        detail: "The exact μ-engine decides by fixpoint iteration; its cost is the reachable \
                 DIAMETER, which bit-count cannot see (`bit-count ≠ tractability`). A wide in-cone \
                 down-counter bounds a descent of up to 2^W steps past the iteration budget, so the \
                 engine abstains after grinding rather than on admission. The decidable lever for a \
                 well-founded descent is a RANKING certificate (a `verify-recoverability` \
                 escalation / the recoverability rescue), not register reduction or input \
                 concretization — and if the descent is not well-founded toward the target, the \
                 property is a genuine diameter wall (an honest ⊥)."
            .into(),
        items: Vec::new(),
    }
}

/// mununu#462 — the bit-blast OOM Skip note: a property whose BDD bit-blast exhausted its node
/// arena mid-evaluation (not at admission), so the engine ABSTAINED (Skipped) rather than
/// `.unwrap()`-panicking / SIGABRT-ing and taking the WHOLE run — and every OTHER property still
/// reports. Names the cone's state-bit width and points at the width lever (`--param`, which
/// re-elaborates with a smaller parameterised counter/register so its bits fit) and the fallback
/// (`--engine explicit`). The state-bit-count analogue of `diameter_bound_skip_note`. Advisory
/// only (a note, never a verdict change).
fn bitblast_oom_skip_note(name: &str, cone_bits: u32) -> VerificationNote {
    let cone = if cone_bits > 0 {
        format!("~{cone_bits}-bit state cone")
    } else {
        "wide state cone".to_string()
    };
    VerificationNote {
        kind: "skip-bitblast-oom".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{name}`: Skipped — the BDD bit-blast exhausted its node arena on a {cone} (a SIZE \
             threshold, not a malformed input); the engine abstained on this property so every \
             other property in the run still reports."
        ),
        detail: "The symbolic engine bit-blasts a property's cone-of-influence into a BDD; a wide \
                 counter/register cone (e.g. several 32-bit counters, each doubled by its `$past` \
                 shadow) can outgrow the node budget mid-fixpoint. This is a size threshold — \
                 shrink the state and it decides. The direct lever is `--param NAME=VALUE`, which \
                 re-elaborates the design with a smaller width parameter (e.g. `--param \
                 BW_WIDTH=4`) so the counter bits fit the arena; the verdict is then scoped to that \
                 parameter value. The engine-level fallback is `--engine explicit`. Only this \
                 property abstained — the rest of the run is unaffected."
            .into(),
        items: Vec::new(),
    }
}

// ---- P2.2d: cost-annotated plan telemetry (per-property routing rationale) --------------------
//
// The planner's per-property cost reasoning, made explicit: from the measured facts
// (property-class × cone-vs-cap × diameter proxy) predict WHICH engine will decide each property
// and WHY. Telemetry — a `plan-cost` note per property + one `plan-accuracy` summary — NEVER a
// verdict change (the precision ladder + reactive rescue still decide). See
// `.claude/plans/cost-annotated-plan-telemetry.md` (P-1 / option 2a). This is the article's
// "watch the planner reason about cost" spine and the agent/PWA loop's actionable "why + what next".

/// One property's predicted routing + rationale (P2.2d). A prediction, not a verdict.
#[derive(Debug, Clone)]
pub struct RoutingRationale {
    /// Property name.
    pub property: String,
    /// Fixpoint class (safety / reachability / liveness-νμ / mixed / propositional).
    pub class: crate::mu_calculus::PropertyClass,
    /// The exact engine's cone-of-influence bit width for the property's atoms.
    pub cone_bits: u32,
    /// The exact engine's effective bit cap for that cone.
    pub cap: u32,
    /// The structural diameter proxy — the widest in-cone down-counter (`Some(W)` ⇒ ~2^W
    /// diameter), or `None`. Free/static (`ModelFacts::cone_counter_diameter_log2`).
    pub diameter_log2: Option<u32>,
    /// The engine the planner predicts will decide this property.
    pub predicted_engine: &'static str,
    /// The one-line rationale.
    pub why: String,
}

/// The DECISION TABLE — the measured engine-routing classification, made explicit. Pure function of
/// the facts; returns `(predicted_engine, why)`. `"⊥"` means no engine is predicted to decide it.
fn predict_engine(
    class: crate::mu_calculus::PropertyClass,
    cone_bits: u32,
    cap: u32,
    diameter_log2: Option<u32>,
) -> (&'static str, String) {
    use crate::mu_calculus::PropertyClass;
    let fits = cone_bits <= cap;
    match class {
        PropertyClass::Safety | PropertyClass::Reachability => {
            if fits {
                (
                    "exact-symbolic",
                    format!(
                        "safety/reachability, cone {cone_bits}b ≤ {cap}b cap → exact decides \
                         `bad`-reachability definitely"
                    ),
                )
            } else {
                (
                    "reach-portfolio",
                    format!(
                        "safety/reachability, cone {cone_bits}b > {cap}b cap → exact over-cap; the \
                         reachability portfolio (native BMC / spacer) decides wide `bad`-reachability"
                    ),
                )
            }
        }
        PropertyClass::Liveness => match (fits, diameter_log2) {
            (true, None) => (
                "exact-symbolic",
                format!(
                    "recoverability/liveness, small control cone {cone_bits}b ≤ {cap}b cap, no wide \
                     counter → exact decides the branching fixpoint"
                ),
            ),
            (true, Some(w)) => (
                "ranking",
                format!(
                    "recoverability/liveness, cone fits but a {w}-bit in-cone counter (~2^{w} \
                     diameter) → exact abstains on the iteration budget; a ranking certificate \
                     decides a well-founded descent, else an honest ⊥"
                ),
            ),
            (false, _) => (
                "cube",
                format!(
                    "recoverability/liveness, cone {cone_bits}b > {cap}b cap → exact over-cap; cube \
                     predicate-abstraction (a register-dominated / value-dependent recovery is an \
                     honest ⊥)"
                ),
            ),
        },
        PropertyClass::Mixed | PropertyClass::Propositional => (
            "exact-symbolic",
            format!("{class:?} property, cone {cone_bits}b → exact-symbolic decides directly"),
        ),
    }
}

/// Compute the routing rationale for each `(name, mu-formula)` property from the design's facts —
/// the planner's cost reasoning, made explicit. Pure over `(btor2, formulas)`; runs NO engine (the
/// diameter is the free structural proxy). A formula that fails to parse is skipped (its own Skip
/// provenance covers it).
pub fn annotate_routing(
    design_btor2: &str,
    properties: &[(String, String)],
) -> Vec<RoutingRationale> {
    use crate::adapter::btor2::model_facts::ModelFacts;
    use crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms;
    use crate::mu_calculus::parser as mu_parser;

    let Ok(file) = crate::adapter::btor2::parser::parse(design_btor2) else {
        return Vec::new();
    };
    let facts = ModelFacts::new(&file);
    let mut out = Vec::new();
    for (name, formula_str) in properties {
        let Ok(formula) = mu_parser::parse(formula_str) else {
            continue;
        };
        let class = formula.property_class();
        let atoms = formula_seed_atoms(&formula);
        let (cone_bits, cap) = facts.cone_vs_cap(&atoms);
        let diameter_log2 = facts.cone_counter_diameter_log2(&atoms);
        let (predicted_engine, why) = predict_engine(class, cone_bits, cap, diameter_log2);
        out.push(RoutingRationale {
            property: name.clone(),
            class,
            cone_bits,
            cap,
            diameter_log2,
            predicted_engine,
            why,
        });
    }
    out
}

/// Render one rationale as a `plan-cost` telemetry note.
fn routing_rationale_note(r: &RoutingRationale) -> VerificationNote {
    let cmp = if r.cone_bits <= r.cap { "≤" } else { ">" };
    let diam = r
        .diameter_log2
        .map(|w| format!(", {w}-bit in-cone counter"))
        .unwrap_or_default();
    VerificationNote {
        kind: "plan-cost".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{}` ({:?}, cone {}b {} {}b cap{}) → predicted `{}`: {}",
            r.property, r.class, r.cone_bits, cmp, r.cap, diam, r.predicted_engine, r.why
        ),
        detail: format!(
            "The planner's per-property cost prediction (telemetry, NOT a verdict): from the \
             measured facts — property class, cone-of-influence bits vs the exact engine's bit cap, \
             and the in-cone diameter proxy — the operator most likely to decide this property is \
             `{}`. The precision ladder + reactive rescue still run unchanged; this only explains \
             the expected routing.",
            r.predicted_engine
        ),
        items: Vec::new(),
    }
}

/// P2.2d self-check — compare each property's PREDICTED decidability to its ACTUAL outcome (a
/// property with per-property engine provenance is not recorded, so the check is predicted-decidable
/// vs actually-decided: `Holds`/`Violated` = decided; `Unknown`/`Skipped` = not). A telemetry
/// signal (and a regression guard against decision-table drift), never an error.
fn plan_accuracy_note(
    rationales: &[RoutingRationale],
    report: &AutoVerifyReport,
) -> Option<VerificationNote> {
    if rationales.is_empty() {
        return None;
    }
    let mut matched = 0usize;
    let mut total = 0usize;
    let mut divergences: Vec<String> = Vec::new();
    for r in rationales {
        let Some(prop) = report.properties.iter().find(|p| p.name == r.property) else {
            continue;
        };
        total += 1;
        let predicted_decidable = r.predicted_engine != "⊥";
        let actually_decided = matches!(
            prop.outcome,
            VerifyOutcome::Holds | VerifyOutcome::Violated { .. }
        );
        if predicted_decidable == actually_decided {
            matched += 1;
        } else {
            divergences.push(format!(
                "{}: predicted `{}` ({}), outcome {}",
                r.property,
                r.predicted_engine,
                if predicted_decidable {
                    "decidable"
                } else {
                    "⊥"
                },
                match &prop.outcome {
                    VerifyOutcome::Holds => "HOLDS".to_string(),
                    VerifyOutcome::Violated { .. } => "VIOLATED".to_string(),
                    VerifyOutcome::Unknown { .. } => "⊥".to_string(),
                    VerifyOutcome::Skipped { .. } => "Skipped".to_string(),
                }
            ));
        }
    }
    if total == 0 {
        return None;
    }
    Some(VerificationNote {
        kind: "plan-accuracy".into(),
        level: NoteLevel::Info,
        summary: format!(
            "planner cost prediction: {matched}/{total} properties matched (predicted-decidable vs \
             actually-decided)"
        ),
        detail: if divergences.is_empty() {
            "Every property's predicted routing matched its outcome — the cost model's decision \
             table agrees with the engines on this design."
                .to_string()
        } else {
            "Divergences below (a telemetry signal, not an error — the prediction is a hint the \
             engines are free to beat)."
                .to_string()
        },
        items: divergences,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts_with(portfolio: Option<PortfolioMode>, sym: bool, exact: bool) -> VerifyAutoOptions {
        VerifyAutoOptions {
            portfolio,
            symbolic_engine: sym,
            exact_symbolic: exact,
            ..Default::default()
        }
    }

    #[test]
    fn plan_portfolio_sequential_emits_the_precision_ladder() {
        let o = opts_with(Some(PortfolioMode::Sequential), false, false);
        // No prepared model in the unit test → the ladder alone (empty rationale).
        let p = plan(None, &o);
        assert_eq!(p.mode, PortfolioMode::Sequential);
        assert_eq!(
            p.ops.iter().map(|e| e.label).collect::<Vec<_>>(),
            vec!["exact-symbolic", "symbolic", "explicit"],
            "portfolio must plan the exact-first precision ladder"
        );
    }

    #[test]
    fn plan_portfolio_parallel_keeps_the_ladder_and_mode() {
        let o = opts_with(Some(PortfolioMode::Parallel), false, false);
        let p = plan(None, &o);
        assert_eq!(p.mode, PortfolioMode::Parallel);
        assert_eq!(p.ops.len(), 3);
    }

    #[test]
    fn plan_single_engine_emits_one_op() {
        for (sym, exact, label) in [
            (false, true, "exact-symbolic"),
            (true, false, "symbolic"),
            (false, false, "explicit"),
        ] {
            let o = opts_with(None, sym, exact);
            let p = plan(None, &o);
            assert_eq!(p.ops.len(), 1, "single-engine intent → one operator");
            assert_eq!(p.ops[0].label, label);
        }
    }

    // ---- P2.2b: residual-register-aware over-cap Skip diagnostic --------------------------

    fn skipped_report(name: &str, formula: &str) -> AutoVerifyReport {
        use crate::adapter::slang::translate::SvaKind;
        use crate::adapter::slang::verify_auto::PropertyVerdict;
        AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: name.into(),
                label: None,
                kind: SvaKind::Assert,
                formula: formula.into(),
                outcome: VerifyOutcome::Skipped {
                    reason: "cone too wide".into(),
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
            }],
            unsupported: Vec::new(),
            diagnostics: Default::default(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn skip_note_register_dominated_says_config_value_cannot_help() {
        // A 300-bit self-incrementing register: the property cone is 300 bits, ALL register
        // (no in-cone inputs), so residual = 300 > the 192-bit cap — pinning inputs can't help.
        let btor2 = "\
1 sort bitvec 300
2 sort bitvec 1
3 state 1 big
4 one 1
5 add 1 3 4
6 next 1 3 5
";
        let report = skipped_report("p_reg", "mu X.(big == 0 || <> X)");
        let notes = skip_over_cap_notes(&report, btor2);
        assert_eq!(
            notes.len(),
            1,
            "an over-cap Skipped property gets one diagnostic"
        );
        let n = &notes[0];
        assert!(
            n.summary.contains("register-dominated"),
            "summary must flag register-domination: {}",
            n.summary
        );
        assert!(
            n.detail.contains("CANNOT") && n.detail.contains("register reduction"),
            "detail must say config-value cannot help + point at register reduction: {}",
            n.detail
        );
    }

    #[test]
    fn skip_note_input_inflated_points_at_config_value() {
        // A 300-bit INPUT feeding a 1-bit register: cone = 301 (1 register + 300 input), so
        // residual = 1 ≤ the 192-bit cap — pinning the input via --config-value decides it.
        let btor2 = "\
1 sort bitvec 300
2 sort bitvec 1
3 input 1 wide
4 state 2 flag
5 zero 1
6 eq 2 3 5
7 next 2 4 6
";
        let report = skipped_report("p_io", "mu X.(flag == 1 || <> X)");
        let notes = skip_over_cap_notes(&report, btor2);
        assert_eq!(
            notes.len(),
            1,
            "an over-cap Skipped property gets one diagnostic"
        );
        let n = &notes[0];
        assert!(
            n.summary.contains("config-value decidable"),
            "summary must flag config-value decidability: {}",
            n.summary
        );
        assert!(
            n.detail.contains("--config-value") && n.detail.contains("INPUT-inflated"),
            "detail must point at --config-value on an input-inflated cone: {}",
            n.detail
        );
    }

    // ---- mununu#462: the bit-blast OOM Skip note ---------------------------------------------

    #[test]
    fn bitblast_oom_skip_emits_size_note_and_spares_other_properties() {
        // A report where ONE property abstained with an "arena exhausted" reason (the bit-blast
        // OOM), alongside a DECIDED property. The classifier emits exactly one `skip-bitblast-oom`
        // note — naming the cone size + `--param` — for the abstained property, and NONE for the
        // decided one: the whole point of mununu#462 is that one over-budget property no longer
        // taints (or crashes) the rest of the run.
        use crate::adapter::slang::translate::SvaKind;
        use crate::adapter::slang::verify_auto::PropertyVerdict;
        // A 300-bit self-incrementing register → a measurable wide cone for `big == 0`.
        let btor2 = "\
1 sort bitvec 300
2 sort bitvec 1
3 state 1 big
4 one 1
5 add 1 3 4
6 next 1 3 5
";
        let report = AutoVerifyReport {
            properties: vec![
                PropertyVerdict {
                    name: "p_wide".into(),
                    label: None,
                    kind: SvaKind::Assert,
                    formula: "mu X.(big == 0 || <> X)".into(),
                    outcome: VerifyOutcome::Skipped {
                        reason: "CEGAR error: symbolic bit-blaster: BDD arena exhausted \
                                 (OxiDD OutOfMemory) — the cone does not compress to a tractable \
                                 BDD; use `--engine explicit`"
                            .into(),
                    },
                    seeded_predicates: Vec::new(),
                    counterexample: None,
                },
                PropertyVerdict {
                    name: "p_ok".into(),
                    label: None,
                    kind: SvaKind::Assert,
                    formula: "big == 0".into(),
                    outcome: VerifyOutcome::Holds,
                    seeded_predicates: Vec::new(),
                    counterexample: None,
                },
            ],
            unsupported: Vec::new(),
            diagnostics: Default::default(),
            notes: Vec::new(),
        };
        let notes = skip_over_cap_notes(&report, btor2);
        let oom: Vec<_> = notes
            .iter()
            .filter(|n| n.kind == "skip-bitblast-oom")
            .collect();
        assert_eq!(
            oom.len(),
            1,
            "exactly one bit-blast-OOM note — for the abstained property only, not the decided one"
        );
        let n = oom[0];
        assert!(
            n.summary.contains("p_wide"),
            "note names the abstained property: {}",
            n.summary
        );
        assert!(
            n.summary.contains("bit") && n.summary.contains("300"),
            "summary names the state-bit size: {}",
            n.summary
        );
        assert!(
            n.detail.contains("--param"),
            "detail points at the --param width lever: {}",
            n.detail
        );
        assert!(
            !notes
                .iter()
                .any(|nn| nn.kind == "skip-bitblast-oom" && nn.summary.contains("p_ok")),
            "a decided property must not receive a bit-blast-OOM note",
        );
    }

    // ---- P2.2c: the diameter-proxy Skip note --------------------------------------------------

    #[test]
    fn skip_note_diameter_bound_flags_in_cone_down_counter() {
        // A recoverability property Skipped whose cone FITS the cap (an 8-bit counter) but carries
        // a down-counter → the abstention is attributed to the ITERATION budget (2^8 diameter),
        // NOT the bit cap. This is the compute_engine_drain shape (proxy fires, exact abstained).
        let btor2 = "\
1 sort bitvec 8
2 sort bitvec 1
3 state 1 cnt
4 ones 1
5 zero 1
6 one 1
7 eq 2 3 5
8 sub 1 3 6
9 ite 1 7 4 8
10 next 1 3 9
11 init 1 3 4
";
        let report = skipped_report("p_drain", "nu Y.((mu X.(cnt == 0 || <> X)) && [] Y)");
        let notes = skip_over_cap_notes(&report, btor2);
        assert_eq!(notes.len(), 1, "the diameter-bound note fires: {notes:?}");
        assert_eq!(notes[0].kind, "skip-diameter-bound");
        assert!(
            notes[0].summary.contains("DIAMETER") && notes[0].summary.contains("8-bit"),
            "summary must attribute the Skip to the 8-bit-counter diameter: {}",
            notes[0].summary
        );
        assert!(
            notes[0].detail.contains("RANKING"),
            "detail must point at the ranking lever, not register reduction: {}",
            notes[0].detail
        );
    }

    #[test]
    fn skip_note_no_diameter_claim_without_a_counter() {
        // A Skipped property over a plain toggle (no counter, cone under cap) gets NO diameter
        // note — the proxy honestly says nothing when it cannot tell (no over-claiming).
        let btor2 =
            "1 sort bitvec 1\n2 state 1 flag\n3 not 1 2\n4 next 1 2 3\n5 zero 1\n6 init 1 2 5\n";
        let report = skipped_report("p_flag", "nu Y.((mu X.(flag == 1 || <> X)) && [] Y)");
        let notes = skip_over_cap_notes(&report, btor2);
        assert!(
            notes.is_empty(),
            "no counter in cone ⇒ no diameter note (honest silence): {notes:?}"
        );
    }

    // ---- P2.2d: cost-annotated plan telemetry -------------------------------------------------

    #[test]
    fn predict_engine_decision_table() {
        use crate::mu_calculus::PropertyClass::*;
        // safety, cone fits the cap → exact decides.
        assert_eq!(predict_engine(Safety, 40, 192, None).0, "exact-symbolic");
        // safety, over cap → the reachability portfolio (BMC/spacer) decides wide safety.
        assert_eq!(predict_engine(Safety, 300, 192, None).0, "reach-portfolio");
        // reachability, fits → exact.
        assert_eq!(
            predict_engine(Reachability, 10, 192, None).0,
            "exact-symbolic"
        );
        // liveness/νμ-recoverability, small control cone, no counter → exact.
        assert_eq!(predict_engine(Liveness, 20, 192, None).0, "exact-symbolic");
        // liveness, cone fits BUT a wide in-cone counter → exact abstains on the diameter → ranking.
        assert_eq!(predict_engine(Liveness, 40, 192, Some(24)).0, "ranking");
        // liveness, over cap (value/register-dominated) → cube (→ honest ⊥).
        assert_eq!(predict_engine(Liveness, 300, 192, None).0, "cube");
    }

    #[test]
    fn annotate_routing_reads_the_facts_and_predicts_ranking_for_a_counter_recovery() {
        // A down-counter recoverability: the cone FITS the cap (8-bit) but carries an in-cone
        // counter → the diameter proxy fires → the planner predicts `ranking` (exact will abstain).
        let btor2 = "1 sort bitvec 8\n2 sort bitvec 1\n3 state 1 cnt\n4 ones 1\n5 zero 1\n\
                     6 one 1\n7 eq 2 3 5\n8 sub 1 3 6\n9 ite 1 7 4 8\n10 next 1 3 9\n11 init 1 3 4\n";
        let props = vec![(
            "p_drain".to_string(),
            "nu Y.((mu X.(cnt == 0 || <> X)) && [] Y)".to_string(),
        )];
        let r = annotate_routing(btor2, &props);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].class, crate::mu_calculus::PropertyClass::Liveness);
        assert_eq!(
            r[0].diameter_log2,
            Some(8),
            "the 8-bit in-cone counter is seen"
        );
        assert_eq!(r[0].predicted_engine, "ranking");
        // The rendered note carries the measured facts inline (article material).
        let note = routing_rationale_note(&r[0]);
        assert_eq!(note.kind, "plan-cost");
        assert!(note.summary.contains("in-cone counter") && note.summary.contains("ranking"));
    }

    #[test]
    fn plan_accuracy_flags_a_predicted_decidable_that_did_not_decide() {
        // Predicted decidable (exact) but the property came back Skipped ⇒ a 0/1 divergence — a
        // telemetry signal, never an error.
        let rationales = vec![RoutingRationale {
            property: "p_drain".into(),
            class: crate::mu_calculus::PropertyClass::Safety,
            cone_bits: 40,
            cap: 192,
            diameter_log2: None,
            predicted_engine: "exact-symbolic",
            why: "x".into(),
        }];
        let report = skipped_report("p_drain", "nu Y.((mu X.(cnt == 0 || <> X)) && [] Y)");
        let note = plan_accuracy_note(&rationales, &report).expect("accuracy note");
        assert_eq!(note.kind, "plan-accuracy");
        assert!(note.summary.contains("0/1"), "summary: {}", note.summary);
        assert_eq!(note.items.len(), 1, "one divergence recorded");
    }
}
