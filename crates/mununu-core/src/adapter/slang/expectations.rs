//! mununu#537 — verdict EXPECTATIONS: assert that a run produced the verdicts you claimed.
//!
//! # Why this is mununu's job and not each consumer's
//!
//! A verification gate never wants "did it pass". It wants **"did exactly this happen"**. Driving
//! mununu across 22 RTL blocks, monono converged on three assertions and rebuilt them in shell —
//! and every project driving mununu will want them, will write them in shell, and will
//! independently rediscover the same traps. The vocabulary is about *verdicts*, which is mununu's
//! domain: nothing in it mentions the consumer's problem space.
//!
//! # The three verbs
//!
//! | verb | meaning | the failure it was born from |
//! |---|---|---|
//! | all-hold (+ count) | every property HOLDS, nothing unsupported/unknown/skipped, and the COUNT is exactly N | a `bind` that stopped binding produces FEWER properties, and a smaller all-green set reads as a clean pass |
//! | violated | the named are VIOLATED **and every other one still HOLDS** | a faulty twin that breaks *everything* teaches nothing about which property covers which fault |
//! | named | each named returns exactly that verdict; an **unnamed** VIOLATED is still a failure | a recorded ⊥ is a claim about the ENGINE, and it must fail when it stops being true |
//!
//! # The fourth verb, which must NOT exist
//!
//! monono wrote `expect_all_hold_except` — name properties allowed to come back undecided, with a
//! reason — and **deleted it**. It was written for `sdram_ctrl`'s three abstaining assertions;
//! those turned out to be **FALSE rather than hard**. The engine had abstained, the excuse made
//! the abstention comfortable, and three wrong assertions sat behind it until they were restated
//! decidably and all nine held at real timings.
//!
//! So there is deliberately no "tolerate undecided" mode here. [`ExpectedVerdict::Unknown`] is the
//! sound version of the same wish: it pins the ⊥ as a **claim** rather than excusing it, and it
//! fails when the claim stops being true. That is not hypothetical — monono's `video_timing` twin
//! pinned `sva_1=UNKNOWN`, mununu#503 made it decidable, and the gate failed on the next run,
//! which is exactly the outcome you want. A verb that accepted "any verdict here is fine" cannot
//! do that, and a tolerated ⊥ actively prevents it.
//!
//! If a genuinely-hard property ever needs an exemption, it should be added deliberately and with
//! its own argument — not reintroduced here as a convenience, because the failure it enabled was
//! not a tooling failure. It was that ⊥ stopped feeling like "not checked".

use super::verify_auto::{AutoVerifyReport, VerifyOutcome};

/// A verdict a caller can claim for a named property.
///
/// Mirrors [`VerifyOutcome`]'s four arms but carries no payload: a caller claims *that* a property
/// is ⊥, never *how many cells* it left undecided — the cell count is an engine detail that moves
/// with refinement, and pinning it would fail on improvements that are not regressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedVerdict {
    Holds,
    Violated,
    Unknown,
    Skipped,
}

impl ExpectedVerdict {
    /// Parse the CLI/API spelling. Case-insensitive, because the transcript prints `HOLDS` in caps
    /// and `skipped` in lower case and a consumer should never have to care which.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "holds" => Some(Self::Holds),
            "violated" => Some(Self::Violated),
            "unknown" | "bottom" | "⊥" => Some(Self::Unknown),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }

    /// The label this expectation matches, matching [`VerifyOutcome::label`].
    pub fn label(self) -> &'static str {
        match self {
            Self::Holds => "holds",
            Self::Violated => "violated",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
        }
    }
}

/// What a caller claims about a run. An empty `Expectations` asserts nothing and is satisfied by
/// anything — the caller declared no claim, so there is none to break.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expectations {
    /// Every property must HOLD, and nothing may be unsupported, unknown or skipped.
    pub all_hold: bool,
    /// The property count must be exactly this. Usable on its own.
    pub count: Option<usize>,
    /// These must be VIOLATED, and **every other property must HOLD**.
    pub violated: Vec<String>,
    /// These must each return exactly the named verdict. Unnamed properties are ignored **except**
    /// that an unnamed VIOLATED is still a failure.
    pub named: Vec<(String, ExpectedVerdict)>,
}

impl Expectations {
    /// True when the caller claimed nothing, so the verdict gate should stay in charge.
    pub fn is_empty(&self) -> bool {
        !self.all_hold && self.count.is_none() && self.violated.is_empty() && self.named.is_empty()
    }
}

/// One unmet claim. `property` is `None` for a run-level claim (the count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectationFailure {
    pub property: Option<String>,
    pub reason: String,
}

/// The outcome of checking a report against a caller's claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectationResult {
    pub satisfied: bool,
    pub failures: Vec<ExpectationFailure>,
}

/// Check a report against what the caller claimed.
///
/// Pure in `(report, expectations)` — no I/O, no toolchain — so the whole semantics is unit
/// testable, which is where the test weight for this feature lives.
///
/// Every claim is evaluated; the result carries **all** failures rather than stopping at the
/// first, because a gate's output is read by someone deciding what to fix.
pub fn evaluate(report: &AutoVerifyReport, exp: &Expectations) -> ExpectationResult {
    let mut failures = Vec::new();
    let find = |name: &str| report.properties.iter().find(|p| p.name == name);

    // --- the count ------------------------------------------------------------------
    // Checked first and independently: a property that silently stopped being produced is
    // invisible to every per-property check below, because there is nothing to check.
    if let Some(want) = exp.count
        && report.properties.len() != want
    {
        failures.push(ExpectationFailure {
            property: None,
            reason: format!(
                "expected {want} propert{}, got {} — a binding that stopped binding produces \
                 FEWER properties, and a smaller all-green set reads as a clean pass",
                if want == 1 { "y" } else { "ies" },
                report.properties.len()
            ),
        });
    }

    // --- all-hold -------------------------------------------------------------------
    if exp.all_hold {
        // An untranslatable assertion is a property that is NOT being checked, so it fails an
        // all-hold claim even though it has no verdict to disagree with.
        for (name, reason) in &report.unsupported {
            failures.push(ExpectationFailure {
                property: Some(name.clone()),
                reason: format!("did not translate, so it is not being checked: {reason}"),
            });
        }
        if report.properties.is_empty() && report.unsupported.is_empty() {
            failures.push(ExpectationFailure {
                property: None,
                reason: "no properties were verified at all — an empty run is not an all-hold run"
                    .to_string(),
            });
        }
        for p in &report.properties {
            if !matches!(p.outcome, VerifyOutcome::Holds) {
                failures.push(ExpectationFailure {
                    property: Some(p.name.clone()),
                    reason: format!("expected holds, got {}", describe(&p.outcome)),
                });
            }
        }
    }

    // --- violated (+ everything else holds) -----------------------------------------
    for name in &exp.violated {
        match find(name) {
            None => failures.push(ExpectationFailure {
                property: Some(name.clone()),
                reason: absent_reason(report, name, "violated"),
            }),
            Some(p) if !matches!(p.outcome, VerifyOutcome::Violated { .. }) => {
                failures.push(ExpectationFailure {
                    property: Some(p.name.clone()),
                    reason: format!("expected violated, got {}", describe(&p.outcome)),
                })
            }
            Some(_) => {}
        }
    }
    if !exp.violated.is_empty() {
        // The other half of the contrast-twin claim: a twin that breaks EVERYTHING teaches
        // nothing about which property covers which fault.
        for p in &report.properties {
            if exp.violated.iter().any(|n| n == &p.name) {
                continue;
            }
            if !matches!(p.outcome, VerifyOutcome::Holds) {
                failures.push(ExpectationFailure {
                    property: Some(p.name.clone()),
                    reason: format!(
                        "not named as violated, so it must still hold, but got {} — a twin that \
                         breaks every property does not show which property covers the fault",
                        describe(&p.outcome)
                    ),
                });
            }
        }
    }

    // --- named --------------------------------------------------------------------
    for (name, want) in &exp.named {
        match find(name) {
            None => failures.push(ExpectationFailure {
                property: Some(name.clone()),
                reason: absent_reason(report, name, want.label()),
            }),
            Some(p) if p.outcome.label() != want.label() => failures.push(ExpectationFailure {
                property: Some(p.name.clone()),
                reason: format!("expected {}, got {}", want.label(), describe(&p.outcome)),
            }),
            Some(_) => {}
        }
    }
    if !exp.named.is_empty() {
        // An UNNAMED violated is still a failure. Surprises are never silent: this is what keeps
        // the named form from becoming a way to ignore whatever you did not mention.
        for p in &report.properties {
            if !matches!(p.outcome, VerifyOutcome::Violated { .. }) {
                continue;
            }
            let claimed_violated = exp
                .named
                .iter()
                .any(|(n, w)| n == &p.name && *w == ExpectedVerdict::Violated);
            if !claimed_violated {
                failures.push(ExpectationFailure {
                    property: Some(p.name.clone()),
                    reason: "VIOLATED but not named — an unnamed violation is still a failure"
                        .to_string(),
                });
            }
        }
    }

    ExpectationResult {
        satisfied: failures.is_empty(),
        failures,
    }
}

/// Why a claimed property is not in the report — naming the untranslatable reason when there is
/// one, because "absent" alone sends the reader hunting for something mununu already explained.
fn absent_reason(report: &AutoVerifyReport, name: &str, want: &str) -> String {
    if let Some((_, reason)) = report.unsupported.iter().find(|(n, _)| n == name) {
        format!("expected {want}, but it did not translate: {reason}")
    } else {
        format!(
            "expected {want}, but no property of that name is in the report — check the name, or \
             whether the assertion still binds"
        )
    }
}

/// A verdict rendered for a failure message, carrying the figure that explains it.
fn describe(outcome: &VerifyOutcome) -> String {
    match outcome {
        VerifyOutcome::Holds => "holds".to_string(),
        VerifyOutcome::Violated { false_cells } => format!("violated ({false_cells} cell(s))"),
        VerifyOutcome::Unknown { unknown_cells } => format!("unknown/⊥ ({unknown_cells} cell(s))"),
        VerifyOutcome::Skipped { reason } => format!("skipped ({reason})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::slang::translate::SvaKind;
    use crate::adapter::slang::verify_auto::PropertyVerdict;

    fn report(props: &[(&str, VerifyOutcome)]) -> AutoVerifyReport {
        AutoVerifyReport {
            properties: props
                .iter()
                .map(|(name, outcome)| PropertyVerdict {
                    name: (*name).to_string(),
                    kind: SvaKind::Assert,
                    formula: format!("formula::{name}"),
                    outcome: outcome.clone(),
                    seeded_predicates: Vec::new(),
                    counterexample: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn named(pairs: &[(&str, ExpectedVerdict)]) -> Expectations {
        Expectations {
            named: pairs.iter().map(|(n, v)| ((*n).to_string(), *v)).collect(),
            ..Default::default()
        }
    }

    fn violated(names: &[&str]) -> Expectations {
        Expectations {
            violated: names.iter().map(|n| (*n).to_string()).collect(),
            ..Default::default()
        }
    }

    // ---- all-hold -----------------------------------------------------------------

    #[test]
    fn all_hold_is_satisfied_when_everything_holds() {
        let r = report(&[("a", VerifyOutcome::Holds), ("b", VerifyOutcome::Holds)]);
        let exp = Expectations {
            all_hold: true,
            ..Default::default()
        };
        assert!(evaluate(&r, &exp).satisfied);
    }

    #[test]
    fn all_hold_fails_on_an_undecided_property() {
        // NEGATIVE CONTROL for the verb. ⊥ is "not checked", never "near enough".
        let r = report(&[
            ("a", VerifyOutcome::Holds),
            ("b", VerifyOutcome::Unknown { unknown_cells: 32 }),
        ]);
        let res = evaluate(
            &r,
            &Expectations {
                all_hold: true,
                ..Default::default()
            },
        );
        assert!(!res.satisfied);
        assert_eq!(res.failures.len(), 1);
        assert_eq!(res.failures[0].property.as_deref(), Some("b"));
        assert!(res.failures[0].reason.contains("unknown/⊥ (32 cell(s))"));
    }

    #[test]
    fn all_hold_fails_on_a_skipped_property() {
        // `skipped` never fails the ordinary CI gate (`ci_exit_code` maps it to 0), which is
        // exactly why an all-hold CLAIM must reject it: otherwise a property the engine never
        // evaluated passes the strictest verb the tool offers.
        let r = report(&[(
            "a",
            VerifyOutcome::Skipped {
                reason: "bit cap".into(),
            },
        )]);
        assert!(
            !evaluate(
                &r,
                &Expectations {
                    all_hold: true,
                    ..Default::default()
                }
            )
            .satisfied
        );
    }

    #[test]
    fn all_hold_fails_when_an_assertion_did_not_translate() {
        // An untranslatable assertion has no verdict to disagree with, and that is the point:
        // it is not being checked, so it cannot ride along inside an all-green run.
        let mut r = report(&[("a", VerifyOutcome::Holds)]);
        r.unsupported
            .push(("b".into(), "unsupported binary op: BinaryAnd".into()));
        let res = evaluate(
            &r,
            &Expectations {
                all_hold: true,
                ..Default::default()
            },
        );
        assert!(!res.satisfied);
        assert!(
            res.failures[0].reason.contains("BinaryAnd"),
            "the refusal reason must be carried through — mununu says exactly why, and a harness \
             that drops it reports only that something is 'absent'. Got: {}",
            res.failures[0].reason
        );
    }

    #[test]
    fn all_hold_fails_on_an_empty_run() {
        // Zero properties is vacuously "all hold" and must not be.
        assert!(
            !evaluate(
                &report(&[]),
                &Expectations {
                    all_hold: true,
                    ..Default::default()
                }
            )
            .satisfied
        );
    }

    // ---- count --------------------------------------------------------------------

    #[test]
    fn count_catches_a_binding_that_stopped_binding() {
        // THE failure this verb exists for: fewer properties, all of them green.
        let r = report(&[("a", VerifyOutcome::Holds)]);
        let exp = Expectations {
            all_hold: true,
            count: Some(2),
            ..Default::default()
        };
        let res = evaluate(&r, &exp);
        assert!(
            !res.satisfied,
            "a smaller all-green set must NOT read as a pass"
        );
        assert!(
            res.failures
                .iter()
                .any(|f| f.property.is_none() && f.reason.contains("expected 2 properties, got 1"))
        );
    }

    #[test]
    fn count_is_usable_on_its_own() {
        let r = report(&[("a", VerifyOutcome::Holds), ("b", VerifyOutcome::Holds)]);
        assert!(
            evaluate(
                &r,
                &Expectations {
                    count: Some(2),
                    ..Default::default()
                }
            )
            .satisfied
        );
    }

    // ---- violated -----------------------------------------------------------------

    #[test]
    fn violated_requires_the_named_to_fail_and_the_rest_to_hold() {
        let r = report(&[
            ("twin", VerifyOutcome::Violated { false_cells: 1 }),
            ("other", VerifyOutcome::Holds),
        ]);
        assert!(evaluate(&r, &violated(&["twin"])).satisfied);
    }

    #[test]
    fn violated_fails_when_the_twin_breaks_everything() {
        // A twin that breaks EVERY property teaches nothing about which property covers which
        // fault — the contrast is the whole point of the verb.
        let r = report(&[
            ("twin", VerifyOutcome::Violated { false_cells: 1 }),
            ("other", VerifyOutcome::Violated { false_cells: 4 }),
        ]);
        let res = evaluate(&r, &violated(&["twin"]));
        assert!(!res.satisfied);
        assert_eq!(res.failures[0].property.as_deref(), Some("other"));
    }

    #[test]
    fn violated_fails_when_the_named_property_holds() {
        // The vacuity catch: the twin was supposed to break this property and did not.
        let r = report(&[("twin", VerifyOutcome::Holds)]);
        assert!(!evaluate(&r, &violated(&["twin"])).satisfied);
    }

    // ---- named --------------------------------------------------------------------

    #[test]
    fn named_matches_each_claimed_verdict() {
        let r = report(&[
            ("a", VerifyOutcome::Holds),
            ("b", VerifyOutcome::Unknown { unknown_cells: 4 }),
            ("ignored", VerifyOutcome::Skipped { reason: "x".into() }),
        ]);
        let exp = named(&[
            ("a", ExpectedVerdict::Holds),
            ("b", ExpectedVerdict::Unknown),
        ]);
        assert!(
            evaluate(&r, &exp).satisfied,
            "an UNNAMED non-violated property is ignored by this verb"
        );
    }

    #[test]
    fn named_unknown_fails_once_the_property_becomes_decidable() {
        // THE reason `=UNKNOWN` exists rather than a "tolerate undecided" verb. monono's
        // video_timing twin pinned `sva_1=UNKNOWN`; mununu#503 made it decidable and the gate
        // failed on the next run — the pin converted an upstream improvement into a signal
        // instead of into silence. A verb that accepted any verdict could not do this.
        let r = report(&[("sva_1", VerifyOutcome::Violated { false_cells: 1 })]);
        let res = evaluate(&r, &named(&[("sva_1", ExpectedVerdict::Unknown)]));
        assert!(
            !res.satisfied,
            "a ⊥ pinned by name is a CLAIM about the engine, and must fail when it stops holding"
        );
    }

    #[test]
    fn named_fails_on_an_unnamed_violation() {
        // Surprises are never silent: this is what stops the named form becoming a way to
        // ignore whatever you did not mention.
        let r = report(&[
            ("a", VerifyOutcome::Holds),
            ("surprise", VerifyOutcome::Violated { false_cells: 2 }),
        ]);
        let res = evaluate(&r, &named(&[("a", ExpectedVerdict::Holds)]));
        assert!(!res.satisfied);
        assert_eq!(res.failures[0].property.as_deref(), Some("surprise"));
        assert!(res.failures[0].reason.contains("not named"));
    }

    #[test]
    fn named_reports_an_absent_property_with_its_refusal_reason() {
        // "absent" alone sends the reader hunting for something mununu already explained.
        let mut r = report(&[("a", VerifyOutcome::Holds)]);
        r.unsupported
            .push(("gone".into(), "dynamic bit-select `sig[idx]`".into()));
        let res = evaluate(&r, &named(&[("gone", ExpectedVerdict::Holds)]));
        assert!(!res.satisfied);
        assert!(res.failures[0].reason.contains("dynamic bit-select"));
    }

    #[test]
    fn named_reports_a_genuinely_missing_property_plainly() {
        let r = report(&[("a", VerifyOutcome::Holds)]);
        let res = evaluate(&r, &named(&[("typo", ExpectedVerdict::Holds)]));
        assert!(!res.satisfied);
        assert!(res.failures[0].reason.contains("no property of that name"));
    }

    // ---- the empty claim, and parsing ----------------------------------------------

    #[test]
    fn no_claim_is_satisfied_by_anything() {
        // An empty `Expectations` must not turn every run into a failure — the caller declared
        // no claim, so there is none to break, and the ordinary verdict gate stays in charge.
        let r = report(&[("a", VerifyOutcome::Violated { false_cells: 1 })]);
        let exp = Expectations::default();
        assert!(exp.is_empty());
        assert!(evaluate(&r, &exp).satisfied);
    }

    #[test]
    fn verdict_spelling_is_case_insensitive() {
        // mununu prints HOLDS in caps and `skipped` in lower case. A consumer got an EMPTY
        // verdict out of a case-sensitive regex once, and its uncovered-counter then did not
        // count the property at all — a guard that stops firing exactly when it is needed.
        for s in ["HOLDS", "holds", "Holds", " holds "] {
            assert_eq!(
                ExpectedVerdict::parse(s),
                Some(ExpectedVerdict::Holds),
                "{s}"
            );
        }
        assert_eq!(
            ExpectedVerdict::parse("UNKNOWN"),
            Some(ExpectedVerdict::Unknown)
        );
        assert_eq!(ExpectedVerdict::parse("⊥"), Some(ExpectedVerdict::Unknown));
        assert_eq!(
            ExpectedVerdict::parse("skipped"),
            Some(ExpectedVerdict::Skipped)
        );
        assert_eq!(ExpectedVerdict::parse("nonsense"), None);
    }

    #[test]
    fn every_failure_is_reported_not_just_the_first() {
        // A gate's output is read by someone deciding what to fix.
        let r = report(&[
            ("a", VerifyOutcome::Unknown { unknown_cells: 1 }),
            ("b", VerifyOutcome::Violated { false_cells: 1 }),
        ]);
        let res = evaluate(
            &r,
            &Expectations {
                all_hold: true,
                count: Some(3),
                ..Default::default()
            },
        );
        assert_eq!(
            res.failures.len(),
            3,
            "one count failure + two property failures"
        );
    }
}
