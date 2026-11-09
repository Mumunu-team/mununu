//! Roadmap J3 — resume an `sv verify-auto` run from the breadcrumb a killed run left behind.
//!
//! [`crate::adapter::partial_json`] writes one NDJSON record per property, flushed before the
//! next property starts, so a run the kernel OOM killer or a CI timeout ends mid-way leaves the
//! verdicts it had reached on disk. This module reads that file back. The **last record per
//! property** is that property's verdict as the killed run knew it: a definite one (`holds` /
//! `violated`) is RESUMED — the row is not re-run, its verdict is carried into the new report
//! and marked `decided_by: resumed` — and every other row (`unknown`, `skipped`, absent from the
//! file) is re-run.
//!
//! **Refusals, and why they are errors rather than notes.** A verdict is a fact about ONE
//! binary. Each record carries the `build` identity of the binary that wrote it
//! ([`crate::build_info::build_tag`]); a breadcrumb written by a different build is refused
//! outright, because the engine may have changed between the two (mununu#715's motivating case:
//! `--version` stayed `0.4.0` for a month while verdicts changed). A record without a `build`
//! (a binary older than this module) and a binary without an identity (built without git and
//! without `MUNUNU_BUILD_SHA`) are refused for the same reason: the equality cannot be checked,
//! and an unverifiable "same binary" is not one. The refusal names the fix.
//!
//! **A verdict is also a fact about ONE run's inputs** (mununu#933). The build check alone let a
//! breadcrumb from design A carry `holds` / `violated` onto every row of design B (or of an edited
//! A) whose name matched — and default names (`ann_guarantee_0`) repeat across designs. Each
//! record therefore also carries the run's [`run_fingerprint`]: a stable hash of every source's
//! name and content, the lift options and every verify option that can change a verdict. A
//! breadcrumb whose fingerprint differs from this run's is refused, like one from another build.
//! The fingerprint guards against the wrong file, not a forger: over the API the client supplies
//! the breadcrumb and could supply any verdict it liked in its own report.
//!
//! **A property whose records disagree is not resumed.** Under a portfolio each member writes its
//! own record; if one wrote `holds` and another `violated`, the merge would have turned that into
//! an engine-contradiction alarm (`BottomReason::EngineContradiction`). Resuming either side would
//! hide an unsoundness, so the row is re-run and named in the report's note.
//!
//! **What a resumed row does not carry.** The breadcrumb holds verdicts only: no counterexample,
//! no vacuity finding, no seeded predicates, no wall. A resumed `violated` row says the property
//! was violated by that build and nothing more; a run without `--resume-from` gives the trace.
//!
//! > Source of truth: [`parse_breadcrumb`] — surface: CLI+API (`sv verify-auto --resume-from FILE`;
//! > `POST /api/v1/sv/verify-auto` `resume_from`, the breadcrumb's content)

use std::collections::BTreeMap;

use crate::adapter::slang::verify_auto::VerifyOutcome;

/// One property's verdict as the breadcrumb's last record carried it, with the build that wrote
/// it and the phase (`main`, `escalated`, `merged`, `resumed`) of that record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumedVerdict {
    pub outcome: VerifyOutcome,
    pub build: String,
    pub phase: String,
}

/// What a breadcrumb lets the next run skip, and what it does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumePlan {
    /// property → the verdict to carry: definite, consistent across its records, same build.
    pub resumed: BTreeMap<String, ResumedVerdict>,
    /// Properties whose records disagreed (`holds` and `violated` both seen): re-run, and named
    /// in the note — the merge's contradiction alarm must not be lost to a resume.
    pub disagreed: Vec<String>,
    /// Properties whose last record is not definite (`unknown` / `skipped`): re-run.
    pub undecided: Vec<String>,
    /// The build every record named (a file with two builds is refused).
    pub build: String,
}

/// Why a breadcrumb cannot be resumed from. Each message names the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeRefusal {
    /// No record at all.
    Empty,
    /// A line that is not a breadcrumb record (`line` is 1-based).
    Malformed { line: usize, reason: String },
    /// A record with no `build` key: written by a binary older than this feature.
    RecordWithoutBuild { line: usize, property: String },
    /// This process registered no build identity, so equality cannot be checked.
    NoCurrentBuild,
    /// The breadcrumb's build is not this binary's.
    BuildMismatch { breadcrumb: String, current: String },
    /// A record with no `run` key: written by a path that recorded no [`run_fingerprint`].
    RecordWithoutRun { line: usize, property: String },
    /// The breadcrumb was written by a run with other inputs (sources, top, options).
    RunMismatch { breadcrumb: String, current: String },
}

impl std::fmt::Display for ResumeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeRefusal::Empty => write!(
                f,
                "the breadcrumb holds no record; nothing to resume — run without --resume-from"
            ),
            ResumeRefusal::Malformed { line, reason } => write!(
                f,
                "line {line} is not a breadcrumb record ({reason}); the file is not one \
                 `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` wrote, or it was edited"
            ),
            ResumeRefusal::RecordWithoutBuild { line, property } => write!(
                f,
                "line {line} (property `{property}`) names no `build`: it was written by a mununu \
                 older than the resume feature, whose verdicts cannot be tied to this binary — run \
                 without --resume-from once; the new breadcrumb carries the build"
            ),
            ResumeRefusal::NoCurrentBuild => write!(
                f,
                "this binary records no build identity (built without git and without \
                 MUNUNU_BUILD_SHA), so a breadcrumb's build cannot be checked against it — rebuild \
                 from a checkout or set MUNUNU_BUILD_SHA / MUNUNU_BUILD_DIRTY at build time"
            ),
            ResumeRefusal::BuildMismatch {
                breadcrumb,
                current,
            } => write!(
                f,
                "the breadcrumb was written by build {} and this binary is build {}: a verdict \
                 from another build is not carried — run without --resume-from",
                short(breadcrumb),
                short(current)
            ),
            ResumeRefusal::RecordWithoutRun { line, property } => write!(
                f,
                "line {line} (property `{property}`) names no `run`: its inputs cannot be tied to \
                 this run's — run without --resume-from once; the new breadcrumb carries them"
            ),
            ResumeRefusal::RunMismatch {
                breadcrumb,
                current,
            } => write!(
                f,
                "the breadcrumb was written by a run with other inputs (run {breadcrumb}; this run \
                 is {current}): a source, the top or a verdict-affecting option differs, and a \
                 verdict about other inputs is not carried — run without --resume-from"
            ),
        }
    }
}

impl std::error::Error for ResumeRefusal {}

/// The first seven characters of a build tag, as `--version` prints it.
pub fn short(build: &str) -> &str {
    let end = build.char_indices().nth(7).map_or(build.len(), |(i, _)| i);
    &build[..end]
}

/// Read a breadcrumb back against this binary's build identity.
///
/// `current_build` is [`crate::build_info::current`] as a [`crate::build_info::build_tag`]; `None`
/// refuses before a line is read. Blank lines are skipped; every other line must be a record
/// with `property`, `outcome` and `build` strings. The last record per property decides; a
/// property that was ever `holds` AND ever `violated` is not resumed.
pub fn parse_breadcrumb(
    text: &str,
    current_build: Option<&str>,
    current_run: &str,
) -> Result<ResumePlan, ResumeRefusal> {
    let current = current_build.ok_or(ResumeRefusal::NoCurrentBuild)?;
    struct Seen {
        last: (String, String),
        holds: bool,
        violated: bool,
    }
    let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
    let mut build: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| ResumeRefusal::Malformed {
                line,
                reason: e.to_string(),
            })?;
        let field = |key: &str| -> Result<String, ResumeRefusal> {
            v.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| ResumeRefusal::Malformed {
                    line,
                    reason: format!("no string `{key}`"),
                })
        };
        let property = field("property")?;
        let outcome = field("outcome")?;
        if !matches!(
            outcome.as_str(),
            "holds" | "violated" | "unknown" | "skipped"
        ) {
            return Err(ResumeRefusal::Malformed {
                line,
                reason: format!("`outcome` is `{outcome}`, not a verdict label"),
            });
        }
        let Some(record_build) = v.get("build").and_then(serde_json::Value::as_str) else {
            return Err(ResumeRefusal::RecordWithoutBuild { line, property });
        };
        if record_build != current {
            return Err(ResumeRefusal::BuildMismatch {
                breadcrumb: record_build.to_string(),
                current: current.to_string(),
            });
        }
        let Some(record_run) = v.get("run").and_then(serde_json::Value::as_str) else {
            return Err(ResumeRefusal::RecordWithoutRun { line, property });
        };
        if record_run != current_run {
            return Err(ResumeRefusal::RunMismatch {
                breadcrumb: record_run.to_string(),
                current: current_run.to_string(),
            });
        }
        build.get_or_insert_with(|| record_build.to_string());
        let phase = v
            .get("phase")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("main")
            .to_string();
        let entry = seen.entry(property).or_insert_with(|| Seen {
            last: (String::new(), String::new()),
            holds: false,
            violated: false,
        });
        entry.holds |= outcome == "holds";
        entry.violated |= outcome == "violated";
        entry.last = (outcome, phase);
    }
    let Some(build) = build else {
        return Err(ResumeRefusal::Empty);
    };
    let mut plan = ResumePlan {
        build,
        ..ResumePlan::default()
    };
    for (property, s) in seen {
        if s.holds && s.violated {
            plan.disagreed.push(property);
            continue;
        }
        let outcome = match s.last.0.as_str() {
            "holds" => VerifyOutcome::Holds,
            // The breadcrumb carries no cell count; `0` is "unknown count", not "no violation".
            "violated" => VerifyOutcome::Violated { false_cells: 0 },
            _ => {
                plan.undecided.push(property);
                continue;
            }
        };
        plan.resumed.insert(
            property,
            ResumedVerdict {
                outcome,
                build: plan.build.clone(),
                phase: s.last.1,
            },
        );
    }
    Ok(plan)
}

/// mununu#933 — the identity of a run's inputs, as 16 hex digits: a stable FNV-1a hash
/// ([`crate::adapter::btor2::minted_expr::fnv1a64`]) over a canonical text of every source's name
/// and content, the lift options, and every verify option that can change a verdict. Maps and sets
/// are written sorted, so two identical runs in two processes agree (`Debug` of a `HashMap` does
/// not). Both structs are destructured without `..`: a new field does not compile until it is
/// placed on one side or the other.
///
/// Left out, because they choose WHICH rows run, not what a row's verdict is: `skip_rows`,
/// `only_rows`, `resume_from`, `breadcrumb_append`, and the fingerprint itself. Not covered: files
/// reached through `extra_include_dirs` (their paths are hashed, their contents are not) and the
/// `MUNUNU_*` budget variables, which can turn a verdict into `unknown` but never flip one.
pub fn run_fingerprint(
    sources: &[(String, String)],
    yosys: &crate::adapter::yosys::YosysOptions,
    opts: &crate::adapter::slang::verify_auto::VerifyAutoOptions,
) -> String {
    use std::fmt::Write as _;
    let mut t = String::new();
    let sorted = |m: &std::collections::HashMap<String, u64>| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort();
        format!("{v:?}")
    };
    for (name, content) in sources {
        let _ = writeln!(t, "source {name:?} {}", content.len());
        t.push_str(content);
        t.push('\n');
    }
    let crate::adapter::yosys::YosysOptions {
        top,
        additional_sources,
        extra_include_dirs,
        params,
        primary_source_path,
        frontend,
        setundef_anyseq,
        setundef_anyconst,
        init_policy_overrides,
        per_module_btor,
        // Where per-module BTOR2 files are written: an output location, not a verdict input.
        per_module_output_dir: _,
        cutpoint_signals,
        merge_clock_domains,
    } = yosys;
    let _ = writeln!(t, "top {top:?}");
    for (name, content) in additional_sources {
        let _ = writeln!(t, "additional {name:?} {}", content.len());
        t.push_str(content);
        t.push('\n');
    }
    let _ = writeln!(
        t,
        "include_dirs {extra_include_dirs:?} params {params:?} primary {primary_source_path:?} \
         frontend {frontend:?} anyseq {setundef_anyseq} anyconst {setundef_anyconst} init \
         {init_policy_overrides:?} per_module {per_module_btor} cutpoints {cutpoint_signals:?} \
         merge_clocks {merge_clock_domains}"
    );
    let crate::adapter::slang::verify_auto::VerifyAutoOptions {
        max_iterations,
        must_edge_inference,
        must_edge_auto,
        gate_reset,
        auto_stub_flops,
        config_values,
        counter_bounds,
        predicate_hints,
        templates,
        templates_files,
        localize,
        symbolic_engine,
        exact_symbolic,
        portfolio,
        rescue_bottom_safety,
        rescue_bottom_liveness,
        rescue_bottom_recoverability,
        replan_defer,
        breadcrumb_append: _,
        skip_rows: _,
        only_rows: _,
        resume_from: _,
        run_fingerprint: _,
        mutation,
        faults,
        antecedent_shadow,
        antecedent_propagate,
        input_state,
        lift_overrides,
        assume_mode,
    } = opts;
    let _ = writeln!(
        t,
        "iter {max_iterations} must {must_edge_inference:?}/{must_edge_auto} gate {gate_reset} \
         stub {auto_stub_flops} pins {} bounds {} hints {predicate_hints:?} localize {localize} \
         symbolic {symbolic_engine} exact {exact_symbolic} portfolio {portfolio:?} rescue \
         {rescue_bottom_safety}/{rescue_bottom_liveness}/{rescue_bottom_recoverability} defer \
         {replan_defer} mutation {mutation:?} faults {faults:?} shadow {antecedent_shadow} \
         propagate {antecedent_propagate} input_state {input_state} overrides {lift_overrides:?} \
         assume {assume_mode:?}",
        sorted(config_values),
        sorted(counter_bounds),
    );
    for tr in templates {
        let mut args: Vec<_> = tr.args.iter().collect();
        args.sort();
        let _ = writeln!(t, "template {:?} {args:?}", tr.template);
    }
    for path in templates_files {
        match std::fs::read(path) {
            Ok(bytes) => {
                let _ = writeln!(t, "templates_file {path:?} {}", bytes.len());
                t.push_str(&String::from_utf8_lossy(&bytes));
                t.push('\n');
            }
            // An unreadable pack fails the run itself; the fingerprint still names it.
            Err(e) => {
                let _ = writeln!(t, "templates_file {path:?} unreadable {:?}", e.kind());
            }
        }
    }
    format!(
        "{:016x}",
        crate::adapter::btor2::minted_expr::fnv1a64(t.as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "e7a5b21c0ffee0000000000000000000000000000";
    const R: &str = "00000000c0ffee00";

    fn rec(property: &str, outcome: &str, phase: &str, build: Option<&str>) -> String {
        crate::adapter::partial_json::breadcrumb_line(
            0,
            property,
            outcome,
            phase,
            None,
            build,
            Some(R),
        )
    }

    #[test]
    fn the_last_record_per_property_decides_and_only_definite_rows_resume() {
        let text = [
            rec("p0", "holds", "main", Some(B)),
            rec("p1", "unknown", "main", Some(B)),
            rec("p1", "violated", "escalated", Some(B)),
            rec("p2", "unknown", "main", Some(B)),
            rec("p3", "skipped", "main", Some(B)),
            String::new(),
        ]
        .join("\n");
        let plan = parse_breadcrumb(&text, Some(B), R).expect("resumable");
        assert_eq!(plan.build, B);
        assert_eq!(plan.resumed.len(), 2, "{plan:?}");
        assert_eq!(plan.resumed["p0"].outcome, VerifyOutcome::Holds);
        assert_eq!(
            plan.resumed["p1"].outcome,
            VerifyOutcome::Violated { false_cells: 0 },
            "the LAST record (escalated) wins over the main pass's unknown"
        );
        assert_eq!(plan.resumed["p1"].phase, "escalated");
        assert_eq!(plan.undecided, vec!["p2", "p3"]);
        assert!(plan.disagreed.is_empty());
    }

    /// Two members disagreeing is the merge's contradiction alarm; a resume must not pick a side.
    #[test]
    fn a_property_whose_records_disagree_is_rerun_not_resumed() {
        let text = [
            rec("p", "holds", "main", Some(B)),
            rec("p", "violated", "main", Some(B)),
        ]
        .join("\n");
        let plan = parse_breadcrumb(&text, Some(B), R).expect("the file itself is fine");
        assert!(plan.resumed.is_empty());
        assert_eq!(plan.disagreed, vec!["p"]);
    }

    #[test]
    fn a_breadcrumb_from_another_build_is_refused() {
        let other = "0123456789abcdef0123456789abcdef01234567";
        let text = rec("p", "holds", "main", Some(other));
        let err = parse_breadcrumb(&text, Some(B), R).unwrap_err();
        assert_eq!(
            err,
            ResumeRefusal::BuildMismatch {
                breadcrumb: other.to_string(),
                current: B.to_string(),
            }
        );
        let msg = err.to_string();
        assert!(msg.contains("0123456") && msg.contains("e7a5b21"), "{msg}");
    }

    /// A dirty build is a different tag from the clean build of the same commit.
    #[test]
    fn a_dirty_tree_is_not_the_clean_build_of_the_same_commit() {
        let dirty = format!("{B}-dirty");
        let text = rec("p", "holds", "main", Some(&dirty));
        assert!(matches!(
            parse_breadcrumb(&text, Some(B), R),
            Err(ResumeRefusal::BuildMismatch { .. })
        ));
        assert!(parse_breadcrumb(&text, Some(&dirty), R).is_ok());
    }

    #[test]
    fn a_record_without_a_build_is_refused_and_named() {
        let text = [
            rec("p0", "holds", "main", Some(B)),
            rec("p1", "holds", "main", None),
        ]
        .join("\n");
        assert_eq!(
            parse_breadcrumb(&text, Some(B), R).unwrap_err(),
            ResumeRefusal::RecordWithoutBuild {
                line: 2,
                property: "p1".to_string(),
            }
        );
    }

    #[test]
    fn a_binary_without_an_identity_refuses_before_reading() {
        let err = parse_breadcrumb("not even json", None, R).unwrap_err();
        assert_eq!(err, ResumeRefusal::NoCurrentBuild);
        assert!(
            err.to_string().contains("MUNUNU_BUILD_SHA"),
            "names the fix"
        );
    }

    #[test]
    fn an_empty_file_and_a_malformed_line_are_refused_with_the_line_number() {
        assert_eq!(
            parse_breadcrumb("\n\n", Some(B), R).unwrap_err(),
            ResumeRefusal::Empty
        );
        let text = format!(
            "{}\n{{\"property\": 3}}",
            rec("p", "holds", "main", Some(B))
        );
        match parse_breadcrumb(&text, Some(B), R).unwrap_err() {
            ResumeRefusal::Malformed { line, reason } => {
                assert_eq!(line, 2);
                assert!(reason.contains("property"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        let text = format!(
            "{{\"property\":\"p\",\"outcome\":\"maybe\",\"build\":\"{B}\",\"run\":\"{R}\"}}"
        );
        assert!(matches!(
            parse_breadcrumb(&text, Some(B), R),
            Err(ResumeRefusal::Malformed { line: 1, .. })
        ));
    }

    #[test]
    fn short_is_the_version_lines_seven_characters() {
        assert_eq!(short(B), "e7a5b21");
        assert_eq!(short("abc"), "abc");
    }

    fn fp(sources: &[(&str, &str)], top: Option<&str>, pins: &[(&str, u64)]) -> String {
        let sources: Vec<(String, String)> = sources
            .iter()
            .map(|(n, c)| (n.to_string(), c.to_string()))
            .collect();
        let yosys = crate::adapter::yosys::YosysOptions {
            top: top.map(str::to_string),
            ..Default::default()
        };
        let opts = crate::adapter::slang::verify_auto::VerifyAutoOptions {
            config_values: pins.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Default::default()
        };
        run_fingerprint(&sources, &yosys, &opts)
    }

    /// mununu#933 — the identity must agree between two identical runs: a `HashMap`'s iteration
    /// order differs per process, so the fingerprint writes it sorted.
    #[test]
    fn the_run_fingerprint_is_stable_and_ignores_map_order() {
        let pins: Vec<(String, u64)> = (0..64).map(|i| (format!("pin{i}"), i)).collect();
        let forward: Vec<(&str, u64)> = pins.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let backward: Vec<(&str, u64)> = forward.iter().rev().copied().collect();
        let a = fp(&[("d.sv", "module d; endmodule")], Some("d"), &forward);
        let b = fp(&[("d.sv", "module d; endmodule")], Some("d"), &backward);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16, "{a}");
    }

    /// Each verdict input moves it: a source's content, the top, a pin value.
    #[test]
    fn the_run_fingerprint_names_the_sources_the_top_and_the_options() {
        let base = fp(
            &[("d.sv", "module d; endmodule")],
            Some("d"),
            &[("rst_ni", 1)],
        );
        let edited = fp(
            &[("d.sv", "module d; wire x; endmodule")],
            Some("d"),
            &[("rst_ni", 1)],
        );
        let other_top = fp(
            &[("d.sv", "module d; endmodule")],
            Some("e"),
            &[("rst_ni", 1)],
        );
        let other_pin = fp(
            &[("d.sv", "module d; endmodule")],
            Some("d"),
            &[("rst_ni", 0)],
        );
        assert_ne!(base, edited, "an edited design is another run");
        assert_ne!(base, other_top);
        assert_ne!(base, other_pin);
    }

    /// The options that pick which rows run are not inputs of any verdict: a resume itself sets
    /// two of them, and must still match the run it resumes.
    #[test]
    fn the_run_fingerprint_ignores_row_selection_and_the_resume_itself() {
        let sources = vec![("d.sv".to_string(), "module d; endmodule".to_string())];
        let yosys = crate::adapter::yosys::YosysOptions::default();
        let plain = crate::adapter::slang::verify_auto::VerifyAutoOptions::default();
        let resuming = crate::adapter::slang::verify_auto::VerifyAutoOptions {
            resume_from: Some("{}".to_string()),
            skip_rows: ["p".to_string()].into_iter().collect(),
            only_rows: Some(["q".to_string()].into_iter().collect()),
            breadcrumb_append: true,
            run_fingerprint: Some("ffffffffffffffff".to_string()),
            ..Default::default()
        };
        assert_eq!(
            run_fingerprint(&sources, &yosys, &plain),
            run_fingerprint(&sources, &yosys, &resuming)
        );
    }

    #[test]
    fn a_breadcrumb_from_another_run_or_without_a_run_is_refused() {
        let other = crate::adapter::partial_json::breadcrumb_line(
            0,
            "ann_guarantee_0",
            "holds",
            "merged",
            None,
            Some(B),
            Some("1111111111111111"),
        );
        let err = parse_breadcrumb(&other, Some(B), R).unwrap_err();
        assert!(matches!(err, ResumeRefusal::RunMismatch { .. }), "{err:?}");
        assert!(err.to_string().contains("other inputs"), "{err}");
        let none = crate::adapter::partial_json::breadcrumb_line(
            0,
            "p",
            "holds",
            "merged",
            None,
            Some(B),
            None,
        );
        assert_eq!(
            parse_breadcrumb(&none, Some(B), R).unwrap_err(),
            ResumeRefusal::RecordWithoutRun {
                line: 1,
                property: "p".to_string(),
            }
        );
    }
}
