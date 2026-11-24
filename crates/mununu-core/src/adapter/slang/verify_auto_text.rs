//! The user-facing text of `sv verify-auto`: the report notes, the reasons a row was not run or
//! not decided, and the remedies they carry, moved out of [`super::verify_auto`] unchanged so a
//! wording change and a logic change land in different files. A function is here when it
//! returns a note or a reason (`VerificationNote`, its `Option` or `Vec`, or a `*_reason`
//! string) and changes nothing else; the text inside the logic of `verify_auto.rs` stays there.

use super::translate::SvaKind;
use super::verify_auto::*;

/// One per-property `two-state-fold` scope note per construct the translator folded because the
/// model is 2-state (today `$isunknown(x)` → `false`). A property that guards against X then holds
/// trivially; the note keeps that verdict from reading as an X-propagation check.
pub(super) fn two_state_fold_notes(
    folds: &[crate::adapter::slang::translate::TranslationCaveat],
) -> Vec<VerificationNote> {
    folds
        .iter()
        .map(|f| VerificationNote {
            kind: "two-state-fold".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{}: `{}(…)` folded to `false` — the lifted model is 2-state",
                f.assertion, f.construct
            ),
            detail:
                "The design is lifted to a 2-state model (BTOR2 has no X or Z), so no value is \
                     ever unknown and `$isunknown(x)` is `false` on every path. A property such as \
                     `!$isunknown(x)` therefore holds trivially here: the verdict is exact for the \
                     model but says nothing about X-propagation in the RTL, which needs a 4-state \
                     simulator or an X-aware formal tool."
                    .into(),
            items: vec![f.construct.clone()],
            property: Some(f.assertion.clone()),
        })
        .collect()
}

/// mununu#988 / mununu#1001 — the immediate assertions the run did not check, as at most two
/// model-level notes: `dropped-immediate-assertion` for `assert` / `cover` (properties nobody
/// checked) and `dropped-immediate-assume` for `assume` (constraints the model does not apply).
pub(super) fn dropped_immediate_notes(
    sites: &[crate::adapter::slang::translate::ImmediateAssertionSite],
) -> Vec<VerificationNote> {
    let (assumes, checks): (Vec<_>, Vec<_>) = sites.iter().partition(|s| s.kind == SvaKind::Assume);
    let mut notes = Vec::new();
    if !checks.is_empty() {
        notes.push(VerificationNote {
            kind: "dropped-immediate-assertion".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{} immediate assert / cover statement(s) in procedural code were not checked",
                checks.len()
            ),
            detail: "The lift reads the design with `read_slang --ignore-assertions`, which \
                     drops every immediate assertion, and verify-auto extracts only concurrent \
                     ones. These statements are not in the property set: rewrite each as a \
                     concurrent `assert property` / `cover property` (or an `@mununu_guarantee`) \
                     to have it checked."
                .into(),
            items: checks.iter().map(|s| s.describe()).collect(),
            property: None,
        });
    }
    if !assumes.is_empty() {
        notes.push(VerificationNote {
            kind: "dropped-immediate-assume".into(),
            level: NoteLevel::SoundnessCaveat,
            summary: format!(
                "{} immediate assume statement(s) in procedural code were not applied",
                assumes.len()
            ),
            detail: "The model allows the behaviour these assumptions exclude, so it \
                     over-approximates the assumed environment. A universal HOLDS (assert \
                     property, AG, nu) still holds under them. A VIOLATED may be a \
                     counterexample that breaks one of them. An existential or branching HOLDS \
                     (cover, EF, AG EF) does not transfer. Rewrite each as a concurrent `assume \
                     property` to have it applied as a precondition."
                .into(),
            items: assumes.iter().map(|s| s.describe()).collect(),
            property: None,
        });
    }
    notes
}

/// One per-property `strong-sequence-reading` scope note per unbounded `##[m:$]` the translator
/// read as an eventuality. IEEE 1800-2017 makes a sequence in an `assert` weak by default, and a
/// weak unbounded sequence is satisfied by every infinite trace; reading it strongly is what its
/// author almost always means, but it is a choice, so the verdict says so.
pub(super) fn strong_reading_notes(
    readings: &[crate::adapter::slang::translate::TranslationCaveat],
) -> Vec<VerificationNote> {
    readings
        .iter()
        .map(|r| VerificationNote {
            kind: "strong-sequence-reading".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{}: `{}` read as an eventuality (strong), not with the standard's weak default",
                r.assertion, r.construct
            ),
            detail: "IEEE 1800-2017 §16.12.2 makes a sequence in an `assert` weak by default, and a \
                     weak unbounded sequence such as `a |-> ##[1:$] b` is satisfied by every infinite \
                     trace — it can never fail. mununu reads it strongly, as `a |-> s_eventually b` \
                     (AG(a → AF b)), which is what such an assertion is almost always written to \
                     mean. A VIOLATED here is a path on which `b` never arrives; under the weak \
                     reading the same assertion would pass vacuously. Write `s_eventually` or \
                     `strong(…)` to make the intent explicit and remove this note."
                .into(),
            items: vec![r.construct.clone()],
            property: Some(r.assertion.clone()),
        })
        .collect()
}

/// Why an immediate `assume` is in [`AssumptionReport::unapplied`] (mununu#1001).
pub(super) const IMMEDIATE_ASSUME_REASON: &str =
    "an immediate assume in procedural code is not extracted; only `assume property` is applied";

pub(super) fn unseedable_skip_reason(unseedable: &[String], diag: &ModelDiagnostics) -> String {
    let base = format!(
        "atom(s) over non-state signals (combinational/IO — not cube-bindable): {}",
        unseedable.join(", ")
    );
    if !diag.blackboxed_modules.is_empty() {
        format!(
            "{base}. Root cause: {} module(s) instantiated with no body ({}) — \
             registers they drive are not modeled as state. Provide the missing \
             module source(s) to model them.",
            diag.blackboxed_modules.len(),
            diag.blackboxed_modules.join(", ")
        )
    } else if diag.state_register_count == 0 {
        format!(
            "{base}. Root cause: the lifted model has no state registers — the design's \
             state may have been optimized away or cut."
        )
    } else {
        base
    }
}

/// mununu#536 — the `coverage-summary` note, computed from a report's CURRENT verdicts.
///
/// Extracted so it can be recomputed AFTER the portfolio merge. It is the one note whose
/// content is a function of the verdicts, which is exactly why it could go stale while every
/// other note stayed true.
pub(super) fn coverage_summary_note(report: &AutoVerifyReport) -> VerificationNote {
    let (mut holds, mut violated, mut unknown, mut skipped) = (0usize, 0usize, 0usize, 0usize);
    for p in &report.properties {
        match p.outcome {
            VerifyOutcome::Holds => holds += 1,
            VerifyOutcome::Violated { .. } => violated += 1,
            VerifyOutcome::Unknown { .. } => unknown += 1,
            VerifyOutcome::Skipped { .. } => skipped += 1,
        }
    }
    VerificationNote {
        kind: "coverage-summary".into(),
        level: NoteLevel::Info,
        summary: format!(
            "{} assertion(s): {holds} definite (HOLDS), {violated} violated, {unknown} unknown (⊥), {skipped} skipped; {} untranslatable",
            report.properties.len(),
            report.unsupported.len(),
        ),
        detail: "A definite verdict transfers to the RTL (see abstraction-posture); ⊥ means the \
                 abstraction is too coarse to decide — an honest 'don't know', not a violation."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// mununu#803 — a VIOLATED row that ships no path says why: the stage of the path finder that
/// declined and its reason. The verdict stands; the path is the explanation, not the evidence.
pub(super) fn witness_miss_note(
    name: &str,
    decided_by: Option<&str>,
    miss: &crate::adapter::reach_rescue::WitnessMiss,
) -> VerificationNote {
    VerificationNote {
        kind: "witness-unavailable".to_string(),
        level: NoteLevel::Info,
        summary: format!("{name}: no counterexample path shipped — {miss}"),
        detail: format!(
            "the verdict stands (decided by {}); the path finders declined at stage `{}`: {}. A \
             VIOLATED without a path is still a VIOLATED; the path is the explanation, not the \
             evidence.",
            decided_by.unwrap_or("unknown"),
            miss.stage,
            miss.reason
        ),
        items: vec![miss.stage.to_string()],
        property: Some(name.to_string()),
    }
}

/// mununu#1257 — a VIOLATED row whose path is a counterexample but not established the first
/// one: the member `by` found it at `depth` cycles, and the search for the shallowest one did
/// not finish (`why`: its stop, and the setting of the limit when there is one). The verdict
/// and the path stand; a reader that takes the path cycle by cycle knows a shorter one may
/// exist. This was a log line.
pub(super) fn witness_not_shortest_note(
    name: &str,
    by: &str,
    depth: usize,
    why: &str,
) -> VerificationNote {
    VerificationNote {
        kind: "witness-not-shortest".to_string(),
        level: NoteLevel::Info,
        summary: format!(
            "{name}: the counterexample path ({by}, {depth} cycles) is not established the \
             shortest — {why}"
        ),
        detail: format!(
            "the verdict stands, and so does the path: it is `{by}`'s own counterexample, {depth} \
             cycles long, confirmed on the concrete simulator before it shipped. `{by}` does not \
             establish that no shorter path reaches the violation, and the search for the first \
             one (native BMC, one query per depth up to {depth}) did not finish: {why}. A shorter \
             path may exist."
        ),
        items: vec![by.to_string()],
        property: Some(name.to_string()),
    }
}

/// mununu#1148 — one note per clock domain whose root the model ticks every step without a
/// driver it can read. SOUNDNESS: right for an independent source (a PLL behind a black box or
/// a cut); for a black-boxed or stubbed clock gate or divider, a net nothing drives, a clock
/// tied to a constant or a net with several drivers (mununu#1180), the lockstep model is
/// neither an over- nor an under-approximation of the real design, so no verdict on the flops
/// it clocks transfers on its own — a soundness caveat, like `blackbox-cut`. Pure over the
/// domains: the seam `sv verify-auto` consumes.
pub(crate) fn clock_root_notes(
    domains: &[crate::adapter::yosys::ClockDomain],
) -> Vec<VerificationNote> {
    use crate::adapter::yosys::{ClockRoot, quoted_list};
    // Why no verdict transfers, per way the source is missing.
    const SOURCE_UNKNOWN: &str = "The clock's real source is not in the lifted design, so the \
        scan cannot tell an independent clock (a PLL — the lockstep model is right) from a cell \
        that gates or divides its input, or from a net nothing drives (the flops never tick). \
        In the last two cases the model is neither an over- nor an under-approximation of the \
        design, and no verdict on the flops this clock drives transfers on its own";
    const NO_EDGE: &str = "A constant has no edge, so the storage cells on it never update on \
        a clock edge in the design (an asynchronous set or reset still changes one that has \
        it), while the model updates them on every step: that is neither an over- nor an \
        under-approximation of the design, and no verdict on those cells transfers on its own";
    const SEVERAL_DRIVERS: &str = "The lifted netlist has more than one driver on this net and \
        does not say which the design resolves to, so the scan cannot tell an independent \
        clock (the lockstep model is right) from a gated or divided one, or from a conflict. \
        The model is then neither an over- nor an under-approximation of the design, and no \
        verdict on the flops this clock drives transfers on its own";
    domains
        .iter()
        .filter_map(|d| {
            // One exhaustive match: a new root kind is a compile error here, not a note with
            // another kind's remedy. `(kind, what the clock is, why, items[1], remedy)`.
            let (kind, what, why, cell, remedy) = match &d.root {
                ClockRoot::Input | ClockRoot::Derived(_) => return None,
                ClockRoot::BlackBox(t) => (
                    "clock-rooted-at-black-box",
                    format!(
                        "clock `{}` is driven by the output of black box `{t}`",
                        d.net
                    ),
                    SOURCE_UNKNOWN,
                    t.clone(),
                    "Provide the source of the cell that drives the clock, or model its gating \
                     as a clock enable on the root clock.",
                ),
                ClockRoot::BlackBoxPort(types) => (
                    "clock-rooted-at-black-box",
                    format!(
                        "clock `{}` is on a port of black box {} (an `inout` port, or a module \
                         with no declaration: it may drive the clock)",
                        d.net,
                        quoted_list(types)
                    ),
                    SOURCE_UNKNOWN,
                    types.join(", "),
                    "Provide the source of the cell that drives the clock, or connect the \
                     clock in the RTL.",
                ),
                ClockRoot::Cut => (
                    "clock-rooted-at-black-box",
                    format!(
                        "clock `{}` is driven by a cut net (the output of a declared or empty \
                         black box, or a cut point)",
                        d.net
                    ),
                    SOURCE_UNKNOWN,
                    "cut net".to_string(),
                    "Provide the black box's source, or remove the cut point from the clock net.",
                ),
                ClockRoot::Undriven => (
                    "clock-undriven",
                    format!(
                        "clock `{}` is driven by nothing: the net has no driver",
                        d.net
                    ),
                    SOURCE_UNKNOWN,
                    String::new(),
                    "Connect the clock in the RTL.",
                ),
                ClockRoot::Constant { value, register } => (
                    "clock-constant",
                    format!("the clock of `{register}` is tied to constant {value}"),
                    NO_EDGE,
                    register.clone(),
                    "Connect the clock in the RTL.",
                ),
                ClockRoot::MultiDriven(types) => (
                    "clock-multi-driven",
                    format!(
                        "clock `{}` is driven by more than one cell ({})",
                        d.net,
                        quoted_list(types)
                    ),
                    SEVERAL_DRIVERS,
                    types.join(", "),
                    "Leave one driver on the clock net in the RTL.",
                ),
            };
            Some(VerificationNote {
                kind: kind.into(),
                level: NoteLevel::SoundnessCaveat,
                summary: format!(
                    "{what} and clocks {} storage cell(s); the model ticks it on every step",
                    d.cells
                ),
                detail: format!(
                    "{why}; the lift accepted it because `--merge-clock-domains` was given. \
                     {remedy}"
                ),
                items: vec![d.net.clone(), cell, d.cells.to_string()],
                property: None,
            })
        })
        .collect()
}

/// The `clock-domains-merged` caveat over [`merged_clock_domain_descriptions`]; `None` when
/// nothing was merged. SOUNDNESS: the merge is an UNDER-approximation of the interleavings, so a
/// HOLDS does not transfer — the note is a soundness caveat on every verb's report, not only on
/// `sv verify-auto`'s (mununu#1181).
pub(crate) fn merged_clock_domains_note(merged: &[String]) -> Option<VerificationNote> {
    let first = merged.first()?;
    Some(VerificationNote {
        kind: crate::adapter::lift_decisions::CLOCK_DOMAINS_MERGED.into(),
        level: NoteLevel::SoundnessCaveat,
        summary: if merged.len() > 1 {
            format!(
                "lift: {} clock domains merged onto one clock — every verdict assumes they \
                 tick together.",
                merged.len()
            )
        } else if first.contains(", derived") {
            // Roadmap B2 — one domain, on a derived clock.
            "lift: a derived clock merged onto the model's clock — every verdict assumes it \
             ticks on every step."
                .to_string()
        } else {
            // mununu#1148 — one domain, on a clock the design holds no driver for; the
            // `clock-rooted-at-black-box` / `clock-undriven` note beside this one names it.
            "lift: a clock with no driver in the lifted design merged onto the model's \
             clock — every verdict assumes it ticks on every step."
                .to_string()
        },
        detail: "`--merge-clock-domains` lifted a multi-clock design onto the model's one \
                 implicit clock. That is an UNDER-approximation of the real interleavings: a \
                 crossing defect that needs one domain to tick while another does not (a \
                 lost pulse, an incoherent multi-bit bus) cannot occur in this model, so a \
                 HOLDS does not transfer to the design. A VIOLATED is a run of the design in \
                 which the domains happen to tick together. A domain marked `derived` (a \
                 gated, divided or muxed clock) ticks here on every step: for a gated clock \
                 that is the run where the enable stays high; a divided clock cannot tick \
                 every step, so neither verdict transfers."
            .to_string(),
        items: merged.to_vec(),
        property: None,
    })
}

/// mununu#1181 — everything a report must say about the clocks of a lift that went through:
/// the `clock-domains-merged` caveat, then one note per clock the lifted design holds no driver
/// for ([`clock_root_notes`]). Empty for a design with one domain on a clock the model can
/// follow. Every verb that can lift under `--merge-clock-domains` puts these in its report:
/// `sv verify-auto` in `notes` (the caveat through `diagnostics.merged_clock_domains`), the
/// other verbs in `lift_notes`. The policy test
/// `every_lift_that_can_merge_clock_domains_reports_them` lists the callers.
pub(crate) fn lift_clock_notes(
    domains: &[crate::adapter::yosys::ClockDomain],
) -> Vec<VerificationNote> {
    merged_clock_domains_note(&merged_clock_domain_descriptions(domains))
        .into_iter()
        .chain(clock_root_notes(domains))
        .collect()
}

/// What `sv verify-auto`'s `control-slice` note claims for a cut when the lift has no clock
/// note. `build_notes` leaves it out under one, and a test asks which by this constant.
pub(crate) const CUT_HOLDS_TRANSFERS: &str = "Because freeing a net only ADDS transitions, for a \
    SAFETY / universal property a definite HOLDS transfers to the concrete RTL (safety + \
    over-approx = sound).";

pub(super) fn build_notes(
    report: &AutoVerifyReport,
    must_edge_posture: &str,
    applied_config_values: &[(String, u64)],
    applied_params: &[(String, String)],
    counter_bounds: &[String],
    cutpoint_signals: &[String],
    posture: &NotePosture,
) -> Vec<VerificationNote> {
    let d = &report.diagnostics;
    let mut notes = Vec::new();

    // Coverage summary (Info) — the honest at-a-glance tally.
    notes.push(coverage_summary_note(report));

    notes.extend(merged_clock_domains_note(&d.merged_clock_domains));
    // The lift re-ran its yosys script (Info). The note is the one every other `sv` verb puts in
    // its `lift_notes` (mununu#1189).
    notes.extend(
        d.lift_retry
            .as_deref()
            .map(crate::adapter::lift_decisions::lift_retry_note),
    );

    // Config concretization (ScopeCaveat) — H.J.b; present only when the user
    // pinned config inputs to constants.
    if !applied_config_values.is_empty() {
        notes.push(VerificationNote {
            kind: "config-concretization".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{} config input(s) pinned to constants — verdicts hold for THESE values, not all configurations.",
                applied_config_values.len()
            ),
            detail: "A wide config input (e.g. a timer threshold) was fixed to a representative \
                     constant so comparisons against it become decidable. The verdicts below are \
                     therefore scoped to this configuration; a different config value could change \
                     them. Re-run with other values to cover more of the configuration space."
                .into(),
            items: applied_config_values
                .iter()
                .map(|(sig, v)| format!("{sig}={v}"))
                .collect(),
            property: None,
        });
    }

    // Parameter overrides (ScopeCaveat) — present only when the user passed `--param` /
    // `params`. The note is the one every other `sv` verb puts in its `lift_notes`
    // (mununu#1189).
    if !applied_params.is_empty() {
        notes.push(crate::adapter::lift_decisions::parameter_override_note(
            applied_params,
        ));
    }

    // Control-slice cut points (ScopeCaveat) — present only when the user cut nets
    // to free `$anyseq` inputs in the lift. This is an OVER-APPROXIMATION: it adds
    // transitions, so a definite HOLDS is sound but a definite VIOLATED may be
    // spurious unless it is guard-independent (the orphaned-FSM-state case).
    if !cutpoint_signals.is_empty() {
        // T3 (property-class honesty, 2026-07-23) — a cutpoint is a PLAIN over-approximation
        // (it adds transitions). That is sound-transferring for a definite HOLDS on a SAFETY /
        // universal property, but NOT for an ALTERNATING fixpoint (νμ recoverability `AG EF`),
        // which collapses under a plain over- or under-approximation (CLAUDE.md soundness table;
        // Bruns–Godefroid): a HOLDS on the cutpointed model does not automatically transfer to
        // the concrete RTL. Only append the νμ caveat when the report actually carries an
        // alternating property (a parse failure ⇒ conservatively omit — the base claim stays
        // accurate for the safety case).
        let has_alternating = report.properties.iter().any(|p| {
            crate::mu_calculus::parser::parse(&p.formula)
                .map(|f| f.alternation_depth() >= 2)
                .unwrap_or(false)
        });
        // mununu#1180, mununu#1189 — the claims below hold while the clocks of the model are
        // the design's. SOUNDNESS: a lift with a clock note (merged domains, or a clock the
        // lifted design holds no driver for, which a `--cutpoint` on a clock net is one way to
        // get) ticks every clock on every step, so the model lacks the runs of the design in
        // which they do not tick together. With freed nets on top it is neither an over- nor an
        // under-approximation, and the note then makes no transfer claim and names the clock
        // notes. Any clock note withdraws the claim: the rule and the names are
        // `lift_decisions`', shared with the other verbs' `control-slice` note.
        let clocks =
            crate::adapter::lift_decisions::clock_note_names(&clock_notes_of_report(report));
        let numu_caveat = if has_alternating {
            " ⚠ But for an ALTERNATING recoverability property (`AG EF good`, νμ) a HOLDS under this \
              cut is sound for the abstract (cutpointed) model ONLY — it does NOT automatically transfer \
              to the concrete RTL, because alternating fixpoints collapse under a plain over-approximation. \
              Decide those on the un-cutpointed cone (raise the bit cap or pin the freed inputs) rather \
              than trusting the cut."
        } else {
            ""
        };
        let conclusion = if clocks.is_empty() {
            format!(
                "{CUT_HOLDS_TRANSFERS}{numu_caveat} A definite VIOLATED is sound only when the \
                 counterexample is guard-independent — the canonical case being an ORPHANED FSM \
                 state (in-degree 0), which no freed guard can make reachable, so `AG EF \
                 <orphaned-state>` stays soundly VIOLATED. Treat any other VIOLATED under cut \
                 points as possibly spurious and re-check without the cut."
            )
        } else {
            format!(
                "On its own, freeing a net only ADDS transitions. But the clocks of this model \
                 are not the design's ({}): it ticks every clock on every step, so it lacks the \
                 runs of the design in which they do not tick together. The model has runs the \
                 design lacks and lacks runs the design has, and no verdict under these cut \
                 points transfers to the concrete RTL on its own. Re-check without the cut \
                 points to take this note out of the report; the clock notes say what the \
                 clocks alone allow.",
                clocks.join(", ")
            )
        };
        notes.push(VerificationNote {
            kind: crate::adapter::lift_decisions::CONTROL_SLICE.into(),
            level: NoteLevel::ScopeCaveat,
            summary: if clocks.is_empty() {
                format!(
                    "{} net(s) cut to free inputs (control-slice) — an over-approximation of \
                     the design.",
                    cutpoint_signals.len()
                )
            } else {
                format!(
                    "{} net(s) cut to free inputs (control-slice) — with the clock notes of \
                     this lift, not an over-approximation of the design.",
                    cutpoint_signals.len()
                )
            },
            detail: format!(
                "Each listed net was replaced by a free `$anyseq` input (Yosys `cutpoint`), so \
                 its datapath fanin drops out of the cone (this is what lets `exact-symbolic` \
                 fit a wide control FSM). {conclusion}"
            ),
            items: cutpoint_signals.to_vec(),
            property: None,
        });
    }

    // Counter-bound seeding (Info) — H.H; present only when a bound `X <= K` was
    // seeded (user-supplied or config-inferred). A HOLDS it unlocks is PROVEN (the
    // bound is a sound partition the SMT must-edges verify), so no scope caveat
    // beyond the config-concretization one above.
    if !counter_bounds.is_empty() {
        notes.push(VerificationNote {
            kind: "counter-bound".into(),
            level: NoteLevel::Info,
            summary: format!(
                "{} counter-bound predicate(s) seeded to refine monotonicity properties.",
                counter_bounds.len()
            ),
            detail: "A monotonicity/increment property over a counter (`cnt_q >= $past(cnt_q)`) is \
                     ⊥ only because the abstraction admits the 32-bit wraparound. Seeding an upper \
                     bound `X <= K` PARTITIONS the state space (it does not assume the bound — the \
                     must-edges verify the reachable cubes stay bounded), excluding the wraparound \
                     state so the verdict can become definite. Sound regardless of K: a wrong bound \
                     only leaves the property at ⊥, never mis-verdicts. (config-inferred) bounds come \
                     from a counter's comparison against a pinned config threshold; (user-supplied) \
                     from --counter-bound / counter_bounds."
                .into(),
            items: counter_bounds.to_vec(),
            property: None,
        });
    }

    // Abstraction posture — A.3: honest about the may-relation actually used.
    match posture {
        NotePosture::Exact => notes.push(VerificationNote {
            kind: "abstraction-posture".into(),
            level: NoteLevel::Info,
            summary: "Exact-symbolic model checking — no abstraction, so a definite verdict is sound and there is no ⊥."
                .into(),
            detail: "Each property is decided over the full bit-blasted state space by ROBDD \
                     fixpoint; a definite HOLDS or VIOLATED transfers to the concrete RTL, and no \
                     property is left ⊥ (the predicate abstraction that would produce ⊥ is not used)."
                .into(),
            items: Vec::new(),
            property: None,
        }),
        NotePosture::Cube => {
            notes.push(VerificationNote {
                kind: "abstraction-posture".into(),
                level: NoteLevel::Info,
                summary: "KMTS may-over-approximation — a definite HOLDS on a safety property is sound."
                    .into(),
                detail: format!(
                    "The may-relation over-approximates the concrete transitions (SMT all-pairs), so \
                     a definite HOLDS transfers to the concrete RTL (safety + over-approximation = \
                     sound); a definite VIOLATED is a real reachable counterexample class. Must-edge \
                     inference: {}.",
                    must_edge_posture,
                ),
                items: Vec::new(),
                property: None,
            });
        }
    }

    // Reset gating (Info).
    if !d.gated_resets.is_empty() {
        notes.push(VerificationNote {
            kind: "reset-gating".into(),
            level: NoteLevel::Info,
            summary: format!("{} reset(s) pinned inactive.", d.gated_resets.len()),
            detail: "The `disable iff (!rst)` guards were dropped and the reset input pinned to \
                     its inactive value, so the body is verified only while out of reset (the \
                     standard formal reset discipline)."
                .into(),
            items: d.gated_resets.clone(),
            property: None,
        });
    }

    // Flop stubs (Info). The note is the one `sv plan` and `sv mutate` put in their
    // `lift_notes` (mununu#1189).
    if !d.auto_provided_stubs.is_empty() {
        notes.push(crate::adapter::lift_decisions::flop_stub_note(
            &d.auto_provided_stubs,
        ));
    }

    // Black-boxed / cut modules (SoundnessCaveat).
    if !d.blackboxed_modules.is_empty() {
        notes.push(VerificationNote {
            kind: crate::adapter::lift_decisions::BLACKBOX_CUT.into(),
            level: NoteLevel::SoundnessCaveat,
            summary: format!(
                "{} module(s) cut (no body) — state behind them is not modeled.",
                d.blackboxed_modules.len()
            ),
            detail: "Registers driven by a cut module become free inputs, so properties over them \
                     are SKIPPED (never given a verdict), not verified. Provide the missing module \
                     source(s) to model that state."
                .into(),
            items: d.blackboxed_modules.clone(),
            property: None,
        });
    }

    // mununu#599 — the note form of each `vacuity` field, derived from the field.
    {
        let mut carrier = AutoVerifyReport {
            properties: report.properties.clone(),
            ..Default::default()
        };
        refresh_vacuity_notes(&mut carrier);
        notes.extend(carrier.notes);
    }

    // mununu#821 — this rebuild regenerates the MODEL-level notes from the diagnostics and the
    // `vacuity` fields; a note a verdict pass pushed on the way here is KEPT: `witness-unavailable`
    // (mununu#803 — the main loop's exact-witness miss and the safety-witness post-pass),
    // `expression-leaf-unresolved` (mununu#717), `engine-isolation` (mununu#543),
    // `array-atom-unsupported` (mununu#518), the `antecedent-*` notes of the fault posture. Only a
    // stale copy of a kind regenerated above is dropped (`vacuous-guarantee` belongs to the carrier).
    // Before this the assignment `report.notes = build_notes(..)` discarded every one of them, so
    // a VIOLATED annotation whose witness monitor declined shipped with no path and no reason.
    let kept: Vec<VerificationNote> = {
        let regenerated: std::collections::HashSet<&str> =
            notes.iter().map(|n| n.kind.as_str()).collect();
        report
            .notes
            .iter()
            .filter(|n| n.kind != "vacuous-guarantee" && !regenerated.contains(n.kind.as_str()))
            .cloned()
            .collect()
    };
    notes.extend(kept);

    notes
}

/// H.5-GR1 — the provenance note for an [`AnnotationScan`]: how many
/// `@mununu_guarantee` properties were merged, which `@mununu_assume` bodies were
/// seen, and any guarantee bodies that failed to parse (surfaced, never dropped).
/// `None` when the source carried no `@mununu` property annotations.
pub(super) fn annotation_note(scan: &AnnotationScan) -> Option<VerificationNote> {
    if scan.guarantees.is_empty()
        && scan.assumes.is_empty()
        && scan.fairness_assumes.is_empty()
        && scan.skipped.is_empty()
    {
        return None;
    }
    let mut items = Vec::new();
    for g in &scan.guarantees {
        match scan.template_forms.iter().find(|(n, _)| *n == g.name) {
            Some((_, form)) => items.push(format!("template {}: {form}", g.name)),
            None => items.push(format!("guarantee {}: {}", g.name, g.formula)),
        }
    }
    for a in &scan.assumes {
        items.push(format!("assume: {a}"));
    }
    for a in &scan.fairness_assumes {
        // mununu#1073 — written at scan time, before any member runs: it states what the
        // assumption binds, not that it was applied. The row's `fair-cycle-rescue` note (or its
        // `violation-without-fairness` reason) says what happened.
        items.push(format!(
            "assume: {} — binds response-shape rows `AG(a → AF b)`, decided under it by the \
             fair-cycle l2s (mununu#477)",
            render_fairness_atom(a)
        ));
    }
    for s in &scan.skipped {
        items.push(format!("skipped: {s}"));
    }
    Some(VerificationNote {
        kind: "annotation-properties".into(),
        level: if scan.skipped.is_empty() {
            NoteLevel::Info
        } else {
            NoteLevel::ScopeCaveat
        },
        summary: format!(
            "{} `@mununu_guarantee` propert{} verified alongside the design's SVA{}.",
            scan.guarantees.len(),
            if scan.guarantees.len() == 1 {
                "y"
            } else {
                "ies"
            },
            if scan.skipped.is_empty() {
                String::new()
            } else {
                format!("; {} unparsable, skipped", scan.skipped.len())
            }
        ),
        detail:
            "`@mununu_guarantee <mu-calculus>` / `@mununu_assume <body>` annotations carry the \
                 mununu-exclusive properties an author adds beyond the design's own SVA (e.g. \
                 assume-guarantee liveness the SVA fragment cannot express). Guarantee bodies \
                 are mu-calculus formulas parsed by the same parser as the translated SVA and \
                 verified through the same pipeline. `@mununu_assume` of the form \
                 `<signal> = <value>` is applied by this path via config concretization. A \
                 temporal `GF <REG op VALUE>` fairness assume is auto-applied for \
                 response-shape guarantees (`AG(a → AF b)`) via the Emerson–Lei fair-cycle l2s \
                 (mununu#477 Option B) — each `GF` assume adds a `fair_seen` monitor latch, \
                 and a `holds` verdict under fairness means the guarantee holds against every \
                 environment schedule satisfying the assumption infinitely often. For \
                 assumptions that do not fit `GF <REG op VALUE>` — a state predicate, a \
                 non-response guarantee shape, or a multi-guarantee-coupled fairness — the \
                 assume is recorded here; use `--fairness` on `btor2 verify-liveness` over \
                 the emitted BTOR2, or model in CTXDSL where the GR(1) game engine \
                 `(⋀ GF envᵢ) → (⋀ GF sysⱼ)` discharges multi-pair assume-guarantee liveness \
                 directly."
                .into(),
        items,
        property: None,
    })
}

/// mununu#939 — why the run would not execute this row, or `None` when it would: resumed from the
/// breadcrumb (the run skips it), in the skip set, or not selected by `only_rows` — the run's own
/// predicates, in its order.
pub(crate) fn row_not_run_reason(
    name: &str,
    label: Option<&String>,
    only_rows: Option<&std::collections::HashSet<String>>,
    skip_rows: &std::collections::HashSet<String>,
    resumed: &std::collections::BTreeMap<String, crate::adapter::resume::ResumedVerdict>,
) -> Option<String> {
    if let Some(rv) = resumed.get(name) {
        return Some(format!(
            "resumed from the breadcrumb ({}); not re-run",
            rv.outcome.label()
        ));
    }
    if skip_rows.contains(name) {
        return Some("in the run's skip set; not attempted".to_string());
    }
    (!row_selected(only_rows, name, label))
        .then(|| "not selected by MUNUNU_VERIFY_AUTO_ONLY_ROWS; not attempted".to_string())
}

/// Why a property's residual outcome is `⊥`. mununu#492 Part B, extended by mununu#553.
///
/// **Carried as a FIELD on [`PropertyVerdict`], not only as a note** — and the distinction is the
/// whole point. A consumer that does not know about a field cannot silently filter it out; a
/// consumer that does not know about a note routinely does. This has existed as a
/// `bottom-reason` NOTE since mununu#492, and the consumer's formal lane discarded notes wholesale, so
/// it never reached the one reader it was written for — while the engine's own abstention string
/// was being dropped in [`merge_portfolio_reports`] under *"Errors contribute nothing."*
///
/// Measured cost of that pair: a consumer's red lane, an hour spent trying budget knobs blind in
/// the wrong order, and two hypotheses refuted — neither of them from the `⊥` itself.
///
/// No longer `Copy`: [`Self::EngineDidNotComplete`] carries the engine's verbatim message.
/// mununu#1121 — the one remedy for an init-less register that exists on `sv verify-auto`
/// (`docs/policies/failure-paths.md` Rule 5). The verb takes no sidecar and no `init` key; the
/// value has to come from the RTL, and the values a reset branch gives are the ones reset-gating
/// recovers (mununu#578) when it is on.
pub(super) const INIT_REMEDY: &str = "To decide this property, give those registers a reset value in the \
     RTL (a reset branch, or a declaration initializer) and keep reset-gating on — it is on by \
     default; `--no-gate-reset` (API `gate_reset: false`) turns it off. `sv verify-auto` cannot \
     take an initial value from the command line or the request today: a mununu limitation.";

impl BottomReason {
    /// One-line rendering for the CLI, carrying the budget name where there is one.
    pub fn one_line(&self) -> String {
        match self {
            Self::SafetyShapeNotReducible => format!(
                "safety-shape-not-reducible — the property's shape is outside the forms the \
                 rescue lane decides ({}). {}",
                crate::adapter::reach_rescue::accepted_safety_shapes(),
                crate::adapter::reach_rescue::SHAPE_REMEDY
            ),
            Self::NoStateModelNonSafety => "no-state-model-non-safety — the design has zero \
                                            state registers; a modelling issue, not an engine cap"
                .to_string(),
            Self::UnclassifiedBottom => "unclassified — cause not established; do NOT read as \
                                         distinct-from-abstain"
                .to_string(),
            Self::EngineDidNotComplete { engine, detail } => {
                format!("engine `{engine}` did not complete on this design: {detail}")
            }
            Self::EngineContradiction { detail } => format!(
                "SOUNDNESS ALARM — portfolio engines returned CONTRADICTING definite verdicts \
                 ({detail}); one engine is unsound on this design. A bigger budget cannot help \
                 and retrying is the wrong response."
            ),
            Self::EngineCrashed { detail } => {
                format!("the engine process CRASHED (not an abstention): {detail}")
            }
            Self::BudgetExpired => "a WALL-CLOCK budget expired — retrying with more time is the \
                 right response here (MUNUNU_PROPERTY_BUDGET_MS for the property as a whole, \
                 MUNUNU_RESCUE_SLICE_MS for the ⊥ re-plan's per-step slice, \
                 MUNUNU_VERIFY_BUDGET_MS for the whole run). HOST-DEPENDENT: the same command on \
                 a quieter or faster machine may DECIDE this property, so treat a gate failure \
                 here as a configuration result, not a verdict about the design"
                .to_string(),
            Self::MemoryCeilingExceeded => "the process-RSS ceiling was exceeded \
                 (MUNUNU_MAX_PROCESS_MEMORY_BYTES) — retrying with more TIME cannot help; more \
                 memory, a smaller cone, or a lower engine tier can. Host-dependent without a \
                 clock: RSS varies with allocator state and neighbouring processes"
                .to_string(),
            Self::UnestablishedInitialState {
                registers,
                gated_resets,
            } => format!(
                "the property's cone touches {} register(s) whose INITIAL VALUE is not \
                 established ({}), with {} pinned inactive. A free cycle-0 state OVER-approximates \
                 reachability, and that licenses a definite HOLDS but never a definite VIOLATED — \
                 so the violation is withheld rather than reported. Since mununu#578 the reset \
                 values that CAN be recovered are established automatically, so a register still \
                 free here is one with no reset value to recover — it HOLDS through reset \
                 (`next = ite(rst, d, q)`). {} A `--cutpoint` is deliberately free and is NOT \
                 counted here.",
                registers.len(),
                named_registers(registers),
                gated_resets.join(", "),
                INIT_REMEDY
            ),
            Self::ViolationOverFreeLeaves { leaves } => format!(
                "the violation was found on a model FREER than the RTL: the property's cone reads \
                 {} undriven net(s) ({}) — an unconnected wire or the output of a module \
                 missing from the source set, free at every cycle. The trace is kept as a MAY-trace: the real \
                 driver may never produce the values it needs, so the violation is withheld \
                 rather than reported. Vendor the missing module or provide the driver to decide \
                 this property; a HOLDS over the same model would have transferred.",
                leaves.len(),
                leaves.join(", ")
            ),
            Self::ViolationOverClockAsData { clocks } => format!(
                "the violation was found on a model that reads {} clock net(s) ({}) as DATA — a \
                 latch enabled by a clock level, or other logic on the clock: the model treats \
                 that read as a free input, not as the clock the flops fire on, so the trace may \
                 need a clock value between edges the hardware never shows. The trace is kept as \
                 a MAY-trace and the violation is withheld rather than reported. A HOLDS over the \
                 same model would have transferred (mununu#1030).",
                clocks.len(),
                clocks.join(", ")
            ),
            Self::ExistentialHoldsOverFreeLeaves { leaves } => format!(
                "the target was reached on a model FREER than the RTL: the property's cone reads \
                 {} undriven net(s) ({}) — an unconnected wire or the output of a module missing \
                 from the source set, free at every cycle — so the run that reaches it may be one \
                 the real driver never produces, and the existential or branching HOLDS (a cover, \
                 an EF, an AG EF) is withheld rather than reported. Vendor the missing module or \
                 provide the driver to decide this property; a universal HOLDS or an existential \
                 refutation over the same model would have transferred.",
                leaves.len(),
                leaves.join(", ")
            ),
            Self::ExistentialHoldsOverClockAsData { clocks } => format!(
                "the target was reached on a model that reads {} clock net(s) ({}) as DATA — a \
                 latch enabled by a clock level, or other logic on the clock: the model treats \
                 that read as a free input, so the run that reaches the target may need a clock \
                 value between edges the hardware never shows, and the existential or branching \
                 HOLDS (a cover, an EF, an AG EF) is withheld rather than reported. Write the \
                 target over registers the clock-level logic does not feed, or model the latch \
                 as a clock enable; a universal HOLDS or an existential refutation over the same \
                 model would have transferred.",
                clocks.len(),
                clocks.join(", ")
            ),
            Self::ViolationWithoutFairness { assumptions } => format!(
                "the violation was found by a member that does not carry the run's fairness \
                 assumption(s) ({}), and the fair-cycle route did not decide the row under them \
                 — its monitor could not bind an atom, or it returned no verdict in its slice. \
                 The counterexample may be a schedule the assumption forbids, so the violation \
                 is withheld rather than reported. Fairness atoms must be single register \
                 comparisons; `--fairness` on `sv verify-liveness` decides one response \
                 directly (mununu#1073).",
                assumptions.join(" && ")
            ),
            Self::UnestablishedInitialCubes {
                free_dimensions,
                cap,
                registers,
            } => format!(
                "the property's predicates have {free_dimensions} FREE initial dimensions (free \
                 inputs, and predicates over {} register(s) with no `init` line: {}), past \
                 the {cap} the cube path enumerates — so its initial cube set was not read and \
                 nothing was decided. An init-less register is FREE at cycle 0 on every engine \
                 (mununu#579). {}",
                registers.len(),
                named_registers(registers),
                INIT_REMEDY
            ),
            Self::ViolationAtUnestablishedInitialCubes {
                cubes,
                host_dependent,
            } => format!(
                "the violation was read ONLY at {} initial cube(s) the cycle-0 filter kept \
                 without a cycle-0 state proved to inhabit them ({}). A cube is dropped on a \
                 proven `Unsat` only — the right direction for a HOLDS, the wrong one for a \
                 VIOLATED: the design may never occupy such a cube at cycle 0 — so the \
                 violation is withheld rather than reported (mununu#1085). {}",
                cubes.len(),
                cubes.join("; "),
                if *host_dependent {
                    "At least one doubt is a clock or a resource bound: a re-run with more of it \
                     may decide this property (each cube above names its own remedy)."
                } else {
                    "No doubt is a clock: a re-run cannot help; each cube above names its own \
                     remedy, or decide the row with another engine."
                }
            ),
            Self::ReportSelfContradiction { detail } => format!(
                "🔴 SOUNDNESS ALARM — this report contradicts ITSELF: {detail}. The two verdicts \
                 cannot both be right, so BOTH are withheld. Retrying is actively wrong, and so is \
                 believing whichever one looks more plausible: mununu#577 taught that expensively, \
                 where the half that looked broken was the correct one. Escalate this rather than \
                 re-running it."
            ),
            Self::NotAttempted => {
                "never attempted — filtered out before any engine ran".to_string()
            }
        }
    }
}

/// mununu#492 Part B — the per-property provenance note that names a residual
/// ⊥'s classification so a consumer's gate can act on the distinction.
pub(super) fn bottom_reason_note(name: &str, reason: BottomReason) -> VerificationNote {
    let (summary, detail) = match reason {
        BottomReason::SafetyShapeNotReducible => (
            format!(
                "`{name}`: residual ⊥ classified as `safety-shape-not-reducible` — the \
                 rescue lane decides only the forms its detail lists, and this AG-safety \
                 property is in none of them (a register compared with another register, a \
                 leaf the monitor compiler rejects, or an outer shape that is not an `AG`). \
                 NOT the same as an engine resource abstain — a property-shape gap."
            ),
            format!(
                "The safety-rescue lane (mununu#492) decides {}. {} A downstream gate should \
                 NOT treat this ⊥ as interchangeable with an engine's resource abstain.",
                crate::adapter::reach_rescue::accepted_safety_shapes(),
                crate::adapter::reach_rescue::SHAPE_REMEDY
            ),
        ),
        BottomReason::NoStateModelNonSafety => (
            format!(
                "`{name}`: residual ⊥ classified as `no-state-model-non-safety` — the design \
                 has zero state registers AND this property is non-Safety (liveness / \
                 recoverability). On a stateless model, `AG EF good` is vacuously true (rule \
                 4) and `AG(a → AF b)` has no lasso to reduce; the ⊥ is a modelling issue, \
                 not an engine cap."
            ),
            "A purely combinational block has no reachable state trajectory beyond the \
             initial (empty-state) point, so non-Safety property classes are either \
             vacuously true (recoverability) or unfulfillable-by-shape (response-liveness's \
             l2s needs state to close a lasso). Either compose the block with an enclosing \
             stateful context (its scheduler / driver), or restrict the property set to \
             tier-1 safety invariants — which the compound-safety rescue (mununu#492) does \
             decide on stateless models."
                .into(),
        ),
        BottomReason::UnclassifiedBottom => (
            format!(
                "`{name}`: residual ⊥ was not classified by the mununu#492 diagnostic. Most \
                 commonly this is a resource abstain (bit-cap / node-budget / iteration / \
                 wall-clock) on the exact-symbolic or cube engine — check for a sibling \
                 `bit-cap` / `abstained on the …` note on the same property."
            ),
            "The bottom-reason classifier recognises three shapes (safety-shape-not-reducible, \
             no-state-model-non-safety, unclassified). Anything outside those falls back to \
             this note. It is NOT a claim about resource vs shape — just an honest 'this \
             classifier did not narrow it further', so the consumer looks at the engine-side \
             notes for the actionable answer."
                .into(),
        ),
        // mununu#553 — the case the sentence just above used to send readers hunting for.
        // That advice ("check for a sibling `abstained on the …` note") was unactionable: the
        // engine's abstention string was being DROPPED at the portfolio merge, so the sibling
        // note it pointed at did not exist. Now the reason arrives here verbatim.
        BottomReason::EngineDidNotComplete { engine, detail } => (
            format!(
                "`{name}`: ⊥ because engine `{engine}` did not complete on this design — \
                 {detail}"
            ),
            format!(
                "The abstention message above is the ENGINE'S OWN and names the budget it hit \
                 plus the numbers, so it is directly actionable — raise the named knob, or route \
                 the property elsewhere.\n\nRead the attribution precisely: `{engine}` failed \
                 over the WHOLE DESIGN, which is not a claim that THIS property caused the \
                 failure. What is claimed is that this property is ⊥ and the engine most likely \
                 to have decided it stopped for the stated reason. (Conflating those is the \
                 mununu#548 defect — a note describing one engine inherited by properties \
                 another engine decided.)\n\nAlso carried as the structured \
                 `bottom_reason` FIELD on the property, which a consumer cannot filter out by \
                 not knowing about it — a note can be, and was, for the reader this was written \
                 for."
            ),
        ),
        // Every variant added after mununu#553 renders from `one_line()`, which the compiler
        // forces to stay exhaustive. So a new variant cannot silently acquire an empty or
        // misleading note — the failure mode that let `unclassified-bottom` point readers at a
        // sibling note the merge had already discarded.
        other => (
            format!("`{name}`: ⊥ — {}", other.one_line()),
            format!(
                "Machine-readable as the `bottom_reason` FIELD on this property (tag `{}`), which \
                 a consumer cannot filter out by not knowing about it. Branch a gate on the TAG: \
                 `budget-expired` is the one case where retrying with more budget is right; \
                 `engine-contradiction` is a SOUNDNESS ALARM where retrying is actively wrong; \
                 `safety-shape-not-reducible` needs the property in one of the forms its note \
                 lists, not a bigger budget.",
                other.tag()
            ),
        ),
    };
    VerificationNote {
        kind: "bottom-reason".into(),
        level: NoteLevel::ScopeCaveat,
        summary,
        detail,
        items: Vec::new(),
        // mununu#548 O-2 — `bottom-reason` is THE per-property note, and the one whose join
        // matters: `drop_stale_bottom_reasons` removes it when another engine went on to decide the
        // property, and a missed match leaves a report saying `p` is ⊥ while also reporting it
        // HOLDS. That is the mununu#548 defect, so this is the site that must not rely on prose.
        property: Some(name.to_string()),
    }
}

/// P2.2a/b — **residual-register-aware** actionable Skip diagnostic (the `ModelFacts`
/// consumer). For each property STILL Skipped after the exact attempt, use `ModelFacts` to
/// check whether the exact engine bailed because the property's cone exceeds the bit cap
/// (`cone_bits > cap`); if so, attach a note naming the cone width and the cap.
///
/// The **key fact** (P2.2b, measure-first 2026-07-26) is the *residual* — the register bits
/// that remain even after pinning **every** in-cone free input
/// (`residual = cone_bits − Σ pinnable_input_widths`). This is what decides whether
/// `--pin` pinning can actually help:
///
/// - `residual ≤ cap` — **input-inflated** cone: pinning the in-cone inputs drops the cone to
///   ~`residual` bits ≤ cap, so `--pin SIGNAL=VALUE` (a scoped verdict) decides it.
///   The note lists the pinnable inputs and how far pinning gets.
/// - `residual > cap` — **register-dominated** cone: even pinning ALL in-cone inputs leaves
///   `residual` register bits over the cap, so `--pin` **cannot** decide it. The note
///   says so and points at the real levers (structural compression: one-hot re-encode /
///   counter abstraction; or cutpoint, posture-permitting).
///
/// A measure-first sweep (i2c 122 / gost 360 / present_cipher 144 residual register bits, all
/// above the 64-bit cap) showed the corpus's over-cap cones are register-dominated — so the
/// earlier "just pin the wide inputs" advice over-promised. This diagnostic tells the truth
/// per property instead of assuming input-inflation. Verdict-equivalent — a diagnostic, never
/// a verdict change.
pub(crate) fn skip_over_cap_notes(
    report: &AutoVerifyReport,
    design_btor2: &str,
) -> Vec<VerificationNote> {
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
            // mununu#577 — `0` here used to mean two different things: "the cone really is
            // zero bits" and "the width could not be established". The note now only claims a
            // width it actually has.
            let cone_bits = crate::mu_calculus::parser::parse(&prop.formula)
                .ok()
                .and_then(|f| facts.cone_vs_cap(&formula_seed_atoms(&f)))
                .map(|(cb, _)| cb);
            notes.push(bitblast_oom_skip_note(&prop.name, cone_bits));
            continue;
        }
        let Ok(formula) = crate::mu_calculus::parser::parse(&prop.formula) else {
            continue;
        };
        let atoms = formula_seed_atoms(&formula);
        // mununu#577 — an UNKNOWN cone width must not read as "fits the cap". Previously
        // `leaf_cells()` swallowed its error into an empty vec, so an array-bearing design
        // measured 0 bits and `cone_bits <= cap` was trivially true — on a nine-register design.
        // With the width unknown there is nothing to say about the cap, so say nothing.
        let Some((cone_bits, cap)) = facts.cone_vs_cap(&atoms) else {
            continue;
        };
        if cone_bits <= cap {
            // Not a bit-CAP issue (the cone fits). Consult the DIAMETER proxy (P2.2c): an in-cone
            // down-counter of width W means the fixpoint may need ~2^W iterations, so exact
            // abstains on the ITERATION budget rather than the bit cap — a different, and more
            // actionable, Skip reason than "cone too wide". Emit a diameter-bound note when the
            // proxy can tell (it covers registers that step themselves, up or down, by any step,
            // mununu#929 — a non-counter diameter is honestly out of its reach); otherwise leave
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
                 (`--pin SIGNAL=VALUE`) drops the cone to ~{residual} register bits (≤ \
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
                 register bits (still > the {cap}-bit cap), so `--pin` CANNOT bring it \
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
            property: None,
        });
    }
    notes
}

/// P2.2c — the diameter-proxy Skip note: a property Skipped with its cone UNDER the bit cap, whose
/// cone carries a `W`-bit down-counter (⟹ up to 2^W fixpoint iterations). It tells the user the
/// abstention was the ITERATION budget (the reachable DIAMETER), not the bit cap — the sound lever
/// here is a well-founded RANKING certificate (a `verify-recoverability` escalation / the
/// recoverability rescue), not register reduction. Advisory only (a note, never a verdict change).
/// Test-only re-exports so the note-invariant test in `adapter::slang::verify_auto` can assert
/// across BOTH modules that emit per-property notes (mununu#548 O-2). Keeping the builders private
/// otherwise is deliberate — they are planner internals.
pub(super) fn diameter_bound_skip_note(name: &str, counter_log2: u32) -> VerificationNote {
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
        property: Some(name.to_string()),
    }
}

/// mununu#462 — the bit-blast OOM Skip note: a property whose BDD bit-blast exhausted its node
/// arena mid-evaluation (not at admission), so the engine ABSTAINED (Skipped) rather than
/// `.unwrap()`-panicking / SIGABRT-ing and taking the WHOLE run — and every OTHER property still
/// reports. Names the cone's state-bit width and points at the width lever (`--param`, which
/// re-elaborates with a smaller parameterised counter/register so its bits fit) and the fallback
/// (`--engine explicit`). The state-bit-count analogue of `diameter_bound_skip_note`. Advisory
/// only (a note, never a verdict change).
pub(super) fn bitblast_oom_skip_note(name: &str, cone_bits: Option<u32>) -> VerificationNote {
    // mununu#577 — `None` is "the width was never established", which is NOT the same claim as
    // "the cone is wide". The old signature took a `u32` and read `0` as the latter, so a model
    // whose leaves could not be computed was described as wide on no evidence.
    let cone = match cone_bits {
        Some(b) if b > 0 => format!("~{b}-bit state cone"),
        _ => "state cone of unestablished width".to_string(),
    };
    VerificationNote {
        kind: "skip-bitblast-oom".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{name}`: Skipped — the BDD bit-blast exhausted its node arena on a {cone} (a SIZE \
             threshold; the input is structurally fine); the engine abstained on this property so every \
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
        property: Some(name.to_string()),
    }
}

/// Provenance note for a cube-Skipped property the exact engine rescued (lever b).
pub(super) fn exact_skip_rescue_note(name: &str, verdict: &str) -> VerificationNote {
    VerificationNote {
        kind: "exact-skip-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the predicate cube could not seed the property (no atoms); the \
             full-state exact-symbolic engine decided it {verdict}"
        ),
        detail: "An atom-less modal property (e.g. no-deadlock `nu X.(<> true && [] X)`) has \
                 no state-cell / combinational atom for the cube to build a dimension from, so \
                 the cube reports Skipped. The exact engine bit-blasts the design and decides \
                 it directly. Sound: the exact engine is the differential oracle — under \
                 reset-gating a definite verdict transfers to the RTL; the property is upgraded \
                 only on a definite Holds/Violated."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// mununu#1063 — the exact engine was left behind on the row's budget: which pass, and the
/// engine's own words. Kind `time-budget-exceeded`, the kind the other budget stops use.
pub(super) fn exact_abandoned_note(name: &str, pass: &str, why: &str) -> VerificationNote {
    VerificationNote {
        kind: "time-budget-exceeded".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!("`{name}`: `exact-symbolic` ({pass}) {why}"),
        detail: format!(
            "mununu#1063 — the per-property budget (`{}`) bounds the exact engine from outside: \
             a single BDD apply cannot be interrupted, so the driver stops waiting for it at \
             the deadline and the row goes to the next member and the re-plan. The stop is a \
             wall clock, so this abstention is host-dependent. A larger budget may let the \
             exact engine finish; `--engine explicit` skips it.",
            crate::adapter::run_budget::PROPERTY_BUDGET_ENV
        ),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

/// Plan 2 / B2 — provenance for a plain `EF p` decided on its dual by the reach portfolio.
pub(super) fn reachability_rescue_note(
    name: &str,
    verdict: &str,
    engines: &[&str],
) -> VerificationNote {
    VerificationNote {
        kind: "reachability-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the reachability portfolio decided it {verdict} \
             on the dual `AG !p` (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "`EF p` holds iff some initial state reaches `p` under some input sequence — exactly \
                 the `bad := p` reachability the portfolio decides. A reachable `bad` is a concrete \
                 path to `p` (HOLDS); a proof that `bad` is unreachable is a proof that `p` is never \
                 reached from reset (VIOLATED, with the target atoms as the witness). Sound in both \
                 directions: every portfolio member is sound and a disagreement raises a \
                 contradiction alarm rather than a verdict."
            .into(),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

pub(super) fn rescue_note(name: &str, verdict: &str, engines: &[&str]) -> VerificationNote {
    VerificationNote {
        kind: "portfolio-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the reachability portfolio decided it \
             {verdict} (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "The safety ⊥ was reduced to a `bad`-monitor and decided symbolically by the \
                 multi-engine reachability portfolio (exact ⊕ native ⊕ interpolation ⊕ btormc ⊕ Pono ⊕ rIC3). \
                 Sound: every portfolio member is sound, and a disagreement would surface a \
                 contradiction alarm rather than a verdict."
            .into(),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

/// A provenance note for a νμ recoverability property rescued from `⊥` — by the exact+COI
/// engine (reset-gated) if its cone fits the bit cap, else the scalable cube + smt-hyper-must
/// path.
pub(super) fn recoverability_rescue_note(
    name: &str,
    verdict: &str,
    engine: crate::adapter::recoverability::RecoverabilityEngine,
) -> VerificationNote {
    use crate::adapter::recoverability::RecoverabilityEngine;
    let rung = match engine {
        RecoverabilityEngine::Exact => "exact+COI",
        RecoverabilityEngine::ExactReachRestricted => "exact+COI, reach-restricted",
        RecoverabilityEngine::CubeLadder => "the cube + smt-hyper-must ladder",
    };
    VerificationNote {
        kind: "recoverability-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the νμ recoverability path (exact+COI \
             first, then cube + smt-hyper-must) decided it {verdict} — by {rung}"
        ),
        detail: "The AG EF (recoverability) ⊥ was escalated: exact+COI decides it directly when \
                 the property's cone fits the bit cap (a wishbone/status control signal whose \
                 cone excludes the datapath), else the sound predicate-cube + smt-hyper-must \
                 path. Definite νμ verdicts transfer (exact = oracle; cube per Bruns–Godefroid / \
                 Shoham–Grumberg); box-AF liveness and safety take their own reductions."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// The trap search ran and certified nothing: what each candidate turned out to be, so the ladder's
/// own ⊥ explanation is not the only trail.
pub(super) fn trap_certificate_search_note(
    name: &str,
    good: &crate::adapter::liveness_rescue::Atom,
    search: &crate::adapter::trap_certificate::TrapSearch,
    slice_expired: bool,
) -> VerificationNote {
    VerificationNote {
        kind: "trap-certificate-search".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: no trap certified for `{} == {}` ({} candidate(s) tried{})",
            good.signal,
            good.value,
            search.tried.len(),
            if slice_expired {
                "; the rescue slice ran out"
            } else {
                ""
            }
        ),
        detail: "A trap is a value of the target's register that is reachable and closed under \
                 every transition. A candidate that is reachable but open, or closed but never \
                 entered, refutes nothing; an undecided one may still be a trap the budget did not \
                 reach. The recoverability ladder ran afterwards, unchanged."
            .into(),
        items: search
            .tried
            .iter()
            .map(|(c, o)| {
                format!(
                    "{} == {} ({}): {}",
                    good.signal,
                    c.value,
                    c.source.label(),
                    o.tag()
                )
            })
            .collect(),
        property: Some(name.to_string()),
    }
}

/// The trap search was not run: the target register's sequential cone holds a free leaf, so a path
/// into a trap could be one the real driver never takes.
pub(super) fn trap_certificate_withheld_note(
    name: &str,
    good: &crate::adapter::liveness_rescue::Atom,
    search: &crate::adapter::trap_certificate::TrapSearch,
) -> VerificationNote {
    VerificationNote {
        kind: "trap-certificate-withheld".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{name}`: no trap certificate attempted — `{}`'s cone reads {} free leaf(s) (an \
             undriven net, a cut point or a black-boxed output)",
            good.signal,
            search.withheld_by.len()
        ),
        detail: "A trap certificate refutes `AG EF p` by a path into a closed set. On a cone with a \
                 free leaf the model over-approximates the RTL: the closure would transfer, but the \
                 path could be one the real driver never takes. Vendor the module, drop the cut \
                 point, or provide the driver, and the search runs."
            .into(),
        items: search.withheld_by.clone(),
        property: Some(name.to_string()),
    }
}

/// Actionable-⊥: an explanation of WHY a νμ recoverability property stayed ⊥ — the structural
/// obstacles in its recovery cone the predicate abstraction cannot cross (uncertified gating
/// counters; wide data/config state the recovery value rides). Purely diagnostic: it never changes
/// the ⊥ verdict, only makes it actionable. The `items` carry the machine-readable obstacle list.
pub(super) fn recoverability_bot_reason_note(
    name: &str,
    diag: &crate::adapter::recoverability::RecoverabilityBotDiagnosis,
) -> VerificationNote {
    let mut items: Vec<String> = Vec::new();
    for (n, w) in &diag.uncertified_counters {
        items.push(format!("uncertified-counter: {n} ({w}-bit)"));
    }
    for (n, w) in &diag.wide_influences {
        items.push(format!("wide-influence: {n} ({w}-bit)"));
    }
    VerificationNote {
        kind: "recoverability-bot-reason".into(),
        // A localized obstacle narrows the verdict's scope (input-scoped / needs an assumption);
        // an empty diagnosis is purely informational.
        level: if diag.is_empty() {
            NoteLevel::Info
        } else {
            NoteLevel::ScopeCaveat
        },
        summary: format!("`{name}`: ⊥ because — {}", diag.hint()),
        detail: "This is a best-effort, sound explanation of the abstain (never a verdict): the \
                 structural obstacles in the recovery cone the predicate cube cannot cross. An \
                 un-certified counter needs a ranking certificate or a fairness assumption; a wide \
                 data/config influence means the recovery value is input-scoped (constrain those \
                 inputs to decide). No obstacle localized ⇒ the abstraction may be closable with \
                 additional predicates."
            .into(),
        items,
        property: None,
    }
}

/// A pass that stopped on a budget names the row it stopped at, under the run-level note's kind
/// (`memory-budget-exceeded` / `time-budget-exceeded`), so a consumer keying on the kind sees it.
pub(super) fn escalation_stopped_note(
    name: &str,
    pass: &str,
    why: EscalationStop,
) -> VerificationNote {
    // mununu#1197 — the closing clause names the setting of the budget that stopped the pass:
    // "raise or disable the budget" named none.
    let (kind, cause, remedy) = match why {
        EscalationStop::Memory(hit) => (
            "memory-budget-exceeded",
            format!(
                "the process memory ceiling was reached ({} B in use, ceiling {} B, {})",
                hit.current_rss_bytes,
                hit.limit_bytes,
                crate::adapter::memory_budget::ceiling_provenance(),
            ),
            format!(
                "the ceiling follows `{}`: raise it, or set it to `0` for no ceiling, to attempt \
                 them",
                crate::adapter::memory_budget::MEMORY_BUDGET_ENV
            ),
        ),
        EscalationStop::Time(set_by) => (
            "time-budget-exceeded",
            "the run's wall-clock budget was reached".to_string(),
            match set_by.setting() {
                Some(setting) => {
                    format!("the budget follows {setting}: raise it to attempt them")
                }
                None => crate::adapter::run_budget::NO_SETTING.to_string(),
            },
        ),
    };
    VerificationNote {
        kind: kind.into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{name}`: the {pass} stopped here because {cause}; this row and every later one it \
             would have attempted keep their earlier verdict"
        ),
        detail: format!(
            "mununu#871 — the stop is a budget, not an engine: the rows it skipped were never \
             attempted by this pass, so their `unknown` / `skipped` says nothing about what the \
             engines can decide; {remedy}."
        ),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

/// mununu#477 Option B — provenance note for a box-AF liveness property the
/// fair-cycle l2s (Emerson–Lei extension) decided under a conjunctive `GF` fairness
/// assumption on primary-input atoms. Names the assumption(s) applied.
pub(super) fn fair_cycle_rescue_note(
    name: &str,
    verdict: &str,
    engines: &[&str],
    fairness_atoms: &[crate::adapter::liveness_rescue::Atom],
) -> VerificationNote {
    use crate::adapter::btor2::predicate_expr::CmpOp;
    let fair_render: Vec<String> = fairness_atoms
        .iter()
        .map(|a| {
            let op = match a.op {
                CmpOp::Eq => "==",
                CmpOp::Ne => "!=",
                CmpOp::Lt => "<",
                CmpOp::Le => "<=",
                CmpOp::Gt => ">",
                CmpOp::Ge => ">=",
            };
            format!("GF ({} {} {})", a.signal, op, a.value)
        })
        .collect();
    let fair_conj = fair_render.join(" && ");
    VerificationNote {
        kind: "fair-cycle-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the fair-cycle l2s (Emerson–Lei) \
             decided it {verdict} under {fair_conj} (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "The box-response liveness ⊥ (`AG(a → AF b)`) was decided under the \
                 conjunctive fairness assumption (mununu#477 Option B) via the Emerson–Lei \
                 fair-cycle l2s: the standard l2s pending / snapshot construction plus one \
                 `fair_seen` monitor latch per `GF` atom, all conjuncted into `bad`. A `holds` \
                 verdict means the guarantee holds against every environment schedule \
                 satisfying every fairness assume infinitely often; a `violated` verdict is \
                 a reachable lasso satisfying every fairness constraint that leaves the \
                 request forever ungranted."
            .into(),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

/// A provenance note for a box-AF liveness property the l2s reduction + reachability
/// portfolio rescued from `⊥`.
pub(super) fn liveness_rescue_note(
    name: &str,
    verdict: &str,
    engines: &[&str],
) -> VerificationNote {
    VerificationNote {
        kind: "liveness-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the liveness-to-safety reduction + \
             reachability portfolio decided it {verdict} (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "The box-response liveness ⊥ (`AG(a → AF b)`) was reduced to a reachable \
                 `b`-free-lasso `bad`-monitor (Biere–Artho–Schuppan liveness-to-safety) and \
                 decided symbolically by the reachability portfolio. Sound + complete for the \
                 box-AF shape; the diamond-EF recoverability response is not reduced here."
            .into(),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

/// mununu#543 W3 — the exact engine died in its isolated child. The text is the one the engine
/// loop held; it moved here when the row's mapping became a function (mununu#1196).
pub(super) fn isolated_engine_died_note(name: &str, why: &str) -> VerificationNote {
    VerificationNote {
        kind: "engine-isolation".to_string(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{}`: the exact-symbolic engine DIED in its isolated child — {why}. \
             The property is `unknown`; every other property in this run is \
             unaffected, which is what isolation buys (mununu#543).",
            name
        ),
        detail: "The engine ran in a child process because \
                 MUNUNU_ISOLATE_ENGINES is set. Without isolation this death \
                 would have ended the whole run with no report at all. The \
                 underlying defect is an unbounded recursion in OxiDD's \
                 `apply_bin` on a malformed diagram; it is not a defect in the \
                 design under verification, and this property's verdict is \
                 absent rather than negative."
            .to_string(),
        items: vec![format!("property:{name}")],
        // mununu#548 O-2 — the property is a FIELD now. `items` keeps its `property:<name>`
        // entry for one release so a consumer already reading it does not break on the same
        // commit that gives them the better route.
        property: Some(name.to_string()),
    }
}

/// mununu#1196 — the parent ended the isolated child before it answered: on the time the
/// property had left, or because the run was cancelled. Kind `time-budget-exceeded`, the kind
/// the in-process engine's budget stop has ([`exact_abandoned_note`]); the engine did not
/// crash, so this is not an `engine-isolation` note.
pub(super) fn isolated_engine_stopped_note(
    name: &str,
    stop: crate::adapter::engine_isolate::IsolatedStop,
) -> VerificationNote {
    use crate::adapter::engine_isolate::IsolatedStop;
    let what = match stop {
        IsolatedStop::BudgetSpent { given_ms: 0 } => {
            "was not started: the property's budget was already spent".to_string()
        }
        IsolatedStop::BudgetSpent { given_ms } => {
            format!("was ended after the {given_ms} ms the property's budget had left")
        }
        IsolatedStop::RunCancelled => {
            "was ended because the run that asked for it was cancelled".to_string()
        }
    };
    VerificationNote {
        kind: "time-budget-exceeded".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!("`{name}`: `exact-symbolic` (first pass, in a child process) {what}"),
        detail: format!(
            "The engine ran in a child process because `{}` is set. The child has the time the \
             property has left (`{}`, within the run's `{}`) and is ended at that deadline, or \
             when the run that asked for the verdict is cancelled. The engine did not crash: \
             the property is `unknown` because its time ran out. The stop is a wall clock, so \
             this abstention is host-dependent. A larger budget may let the engine finish; \
             `--engine explicit` skips it.",
            crate::adapter::engine_isolate::ISOLATE_ENV,
            crate::adapter::run_budget::PROPERTY_BUDGET_ENV,
            crate::adapter::run_budget::RUN_BUDGET_ENV
        ),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}
