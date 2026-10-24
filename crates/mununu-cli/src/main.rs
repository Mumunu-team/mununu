mod loader;
mod render;

use crate::loader::{
    EvalContextParams, PreparedEvalContext, load_context_documents, load_context_documents_mode,
    prepare_eval_context, print_context_structure, realize_documents, validate_preprocessor,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use mununu_core::clts::{Clts, DefaultLabelIdx, LabelId};
use mununu_core::context::{ControllerSynthesis, ControllerSynthesisOptions, DiagnosticsOptions};
use mununu_core::context_dsl::ast::TransitionModalitySpec;
use mununu_core::context_dsl::{ContextDoc, RealizedContext, parse as parse_context_doc};
use mununu_core::mu_calculus::EvaluationOptions;
use render::graph::{
    counterstrategy_to_cytoscape, dsl_automata_to_cytoscape, generate_cytoscape_html,
    unrolled_automata_to_cytoscape,
};
use render::text::{
    render_controller_diagnostics, render_discharge_verdict_text, render_proposal_provenance,
};
use serde::Serialize;
use serde_json::{self, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use tracing_subscriber::{EnvFilter, Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(
    name = "mununu",
    about = "CLTS Verification Tool CLI",
    // mununu#715 — `0.4.0 (e7a5b21)` / `0.4.0 (e7a5b21-dirty)`, or the bare version when the
    // build was not told its commit (`build.rs`).
    version = env!("MUNUNU_VERSION_LINE"),
    disable_help_subcommand = true
)]
struct Cli {
    /// Suppress the `logs/mununu.log` workspace file and the startup log banner —
    /// errors only to stderr (`RUST_LOG` still wins). Use in CI to keep the workspace
    /// clean and the output machine-readable.
    #[arg(long, global = true)]
    quiet: bool,
    /// The deployment configuration (`mununu.toml`: template packs, enabled domains). Overrides
    /// `MUNUNU_DEPLOYMENT_CONFIG`.
    #[arg(long = "deployment-config", global = true, value_name = "FILE")]
    deployment_config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

/// Which verdicts make a verify command exit non-zero — the CI-gate policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum FailOn {
    /// Exit non-zero only when a property is `violated` (default).
    Violated,
    /// Exit non-zero when a property is `violated` OR `unknown` (strict — treat an
    /// undecided verdict as a failure).
    Unknown,
    /// Never exit non-zero on the verdict — always `0` (report-only).
    None,
}

/// CI-gate flags flattened into every verify verb (`--fail-on`).
#[derive(Args, Debug)]
struct CiArgs {
    /// Which verdicts fail the command (non-zero exit) for a CI gate:
    /// `violated` (default) | `unknown` (also fail on undecided) | `none`.
    #[arg(long, value_enum, default_value_t = FailOn::Violated)]
    fail_on: FailOn,
}

/// mununu#541 — machine-readable-output flag, flattened into the `sv` property verbs beside
/// [`CiArgs`].
///
/// **These verbs already print JSON on SUCCESS** (`print_json_summary`), which is why the issue's
/// table saying "structured output: none" is not what a run actually does. What rosf hit was the
/// FLAG: they passed `--json` — the idiom `sv verify-auto` established — and got
/// `error: unexpected argument '--json' found` with exit 2, then reasonably concluded there was no
/// structured output at all.
///
/// So the flag exists for two reasons, and the second is the substantive one:
///
/// 1. The `verify-auto` idiom works on every property verb rather than one.
/// 2. It makes JSON the output on **every exit path, including an error**. Without it a failure
///    prints prose to stderr (measured: *"could not build the liveness monitor — an atom likely
///    binds no signal in the design"*), so a subprocess consumer has to parse two formats and pick
///    by exit code. With it there is one document to read whatever happened.
#[derive(Args, Debug)]
struct JsonArgs {
    /// Emit a single JSON document on stdout for EVERY outcome, errors included.
    ///
    /// Without this flag a successful run already prints JSON but a failure prints prose to stderr.
    /// Pass it when a machine reads the output.
    #[arg(long)]
    json: bool,
}

/// mununu#541 — render an error as the JSON document a `--json` consumer expects, then exit 1.
///
/// Goes to **stdout**, beside the success document, so a consumer reads one stream. The prose also
/// goes to stderr, because a human watching the same run should still see it.
fn exit_with_json_error(verb: &str, file: &str, err: &str) -> ! {
    let doc = serde_json::json!({
        "verb": verb,
        "file": file,
        "error": err,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(text) => println!("{text}"),
        // A serialisation failure here would leave the consumer with nothing at all; the prose on
        // stderr is the fallback.
        Err(e) => eprintln!("failed to serialise the error document: {e}"),
    }
    eprintln!("{err}");
    std::process::exit(1);
}

/// mununu#537 — verdict EXPECTATION flags, flattened into `sv verify-auto` beside [`CiArgs`].
///
/// A verification gate never wants "did it pass"; it wants **"did exactly this happen"**. These
/// are the three assertions every consumer driving mununu rebuilds in shell, moved to where the
/// verdict vocabulary is defined. See [`mununu_core::adapter::slang::expectations`] for the
/// semantics and for why there is deliberately no fourth "tolerate undecided" verb.
#[derive(Args, Debug, Default)]
struct ExpectArgs {
    /// Require EVERY property to hold, with nothing unsupported, unknown or skipped.
    ///
    /// Stricter than `--fail-on unknown`: it also rejects `skipped` (which the CI gate treats as
    /// a pass) and an assertion that did not translate — both are properties that are not being
    /// checked, which is the failure this verb exists to catch.
    #[arg(long, conflicts_with_all = ["expect_violated", "expect"])]
    expect_all_hold: bool,

    /// Require EXACTLY this many properties. Usable on its own.
    ///
    /// A binding that stopped binding produces FEWER properties, and a smaller all-green set
    /// reads as a clean pass. This is the only check that catches it, because a property that
    /// stopped being produced is invisible to every per-property check.
    #[arg(long, value_name = "N")]
    expect_count: Option<usize>,

    /// Comma-separated property names that must be VIOLATED — and every OTHER property must
    /// hold.
    ///
    /// For a contrast twin: a twin that breaks every property teaches nothing about which
    /// property covers which fault, so the "everything else still holds" half is not optional.
    #[arg(long, value_name = "NAMES", value_delimiter = ',')]
    expect_violated: Vec<String>,

    /// `NAME=VERDICT` pairs (`holds` | `violated` | `unknown` | `skipped`, case-insensitive),
    /// comma-separated or repeated.
    ///
    /// Each named property must return exactly that verdict. Unnamed properties are ignored,
    /// EXCEPT that an unnamed VIOLATED is still a failure — surprises are never silent.
    ///
    /// `NAME=unknown` is the sound way to record a ⊥: it pins the abstention as a CLAIM that
    /// fails when the property becomes decidable, rather than excusing it. That is the point —
    /// an upstream improvement should break the pin loudly, not pass in silence.
    ///
    /// mununu#544 — NAME may be the assertion's **SV label** (`a_reseed_only_at_a_span_edge`) or
    /// its positional `<module>_sva_<index>`. **Prefer the label:** it is what the author wrote,
    /// so a reorder is a no-op and a rename is a hard error. An index re-points on any
    /// insertion mid-file, which fails silently — the pin still resolves, to a different
    /// property. The label is tried first. `@mununu_guarantee` properties have no label and
    /// stay positional (`ann_guarantee_<index>`).
    #[arg(long = "expect", value_name = "NAME=VERDICT", value_delimiter = ',')]
    expect: Vec<String>,
}

impl ExpectArgs {
    /// Parse into the core type. `Err` on a malformed `NAME=VERDICT`, which is a usage error
    /// (exit 1) — never a silently dropped claim, which would gate on nothing.
    fn to_expectations(
        &self,
    ) -> Result<mununu_core::adapter::slang::expectations::Expectations, String> {
        use mununu_core::adapter::slang::expectations::{Expectations, ExpectedVerdict};
        let mut named = Vec::new();
        for pair in &self.expect {
            let (name, verdict) = pair.split_once('=').ok_or_else(|| {
                format!("--expect `{pair}`: expected NAME=VERDICT (e.g. `sva_1=unknown`)")
            })?;
            let v = ExpectedVerdict::parse(verdict).ok_or_else(|| {
                format!(
                    "--expect `{pair}`: `{verdict}` is not a verdict — use holds | violated |                      unknown | skipped"
                )
            })?;
            if name.trim().is_empty() {
                return Err(format!("--expect `{pair}`: the property name is empty"));
            }
            named.push((name.trim().to_string(), v));
        }
        Ok(Expectations {
            all_hold: self.expect_all_hold,
            count: self.expect_count,
            violated: self
                .expect_violated
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            named,
        })
    }
}

/// mununu#537 — exit code for "a verdict was not what you claimed".
///
/// Distinct from every existing code on purpose: `1` is a tool/usage error (the run FAILED), `2`
/// is a violated verdict and `3` an undecided one under the ordinary gate. A caller that declared
/// expectations needs to tell "the run broke" apart from "the run worked and disagreed with me",
/// and under `--expect-violated` the ordinary gate would exit 2 on the very violation that was
/// asked for. So when expectations are declared they SUPERSEDE `--fail-on`.
const EXPECTATIONS_UNMET_EXIT: i32 = 4;

/// Exit code for a command line the parser rejects (an unknown flag, a missing argument, a bad
/// value): `EX_USAGE` from `sysexits.h`. Clap's default is `2`, which here means VIOLATED, and
/// rosf read a mistyped flag as a violation that way (mununu#541).
const USAGE_EXIT: i32 = 64;

/// The exit code of a command line the parser did not accept: `0` for a help or version request
/// (not an error), [`USAGE_EXIT`] for every real parse failure, never clap's default `2`.
fn parse_error_exit_code(e: &clap::Error) -> i32 {
    match e.kind() {
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
        _ => USAGE_EXIT,
    }
}

/// Map a single property verdict + the `--fail-on` policy to a process exit code.
/// `0` = pass, `2` = violated, `3` = unknown. (`1` is a tool error, `4` unmet expectations and
/// `64` a usage error, all via `main`.) `holds` / `skipped` / any definite-good verdict is always `0`.
fn ci_exit_code(verdict: &str, fail_on: FailOn) -> i32 {
    match verdict {
        "violated" if matches!(fail_on, FailOn::Violated | FailOn::Unknown) => 2,
        "unknown" if matches!(fail_on, FailOn::Unknown) => 3,
        _ => 0,
    }
}

/// The most severe verdict across many properties (for `verify-auto`):
/// `violated` > `unknown` > everything else. `skipped` counts as pass (a property
/// that was not evaluated is not a failure).
fn worst_verdict<'a>(verdicts: impl IntoIterator<Item = &'a str>) -> &'static str {
    let mut worst = "holds";
    for v in verdicts {
        match v {
            "violated" => return "violated",
            "unknown" => worst = "unknown",
            _ => {}
        }
    }
    worst
}

/// Exit the process with the CI-gate code for `verdict` under `fail_on` (no-op
/// return when the code is `0`, so the caller falls through to `Ok`).
fn ci_gate_exit(verdict: &str, fail_on: FailOn) {
    let code = ci_exit_code(verdict, fail_on);
    if code != 0 {
        std::process::exit(code);
    }
}

#[cfg(test)]
mod ci_gate_tests {
    use super::*;

    #[test]
    fn a_rejected_command_line_exits_64_and_help_exits_0() {
        let code = |argv: &[&str]| match Cli::try_parse_from(argv) {
            Ok(_) => panic!("{argv:?} must not parse"),
            Err(e) => parse_error_exit_code(&e),
        };
        assert_eq!(code(&["mununu", "sv", "verify-auto", "--no-such-flag"]), 64);
        assert_eq!(code(&["mununu", "no-such-command"]), 64);
        assert_eq!(
            code(&["mununu", "context", "eval", "m.ctxdsl"]),
            64,
            "--automaton missing"
        );
        assert_eq!(code(&["mununu", "--help"]), 0);
        assert_eq!(code(&["mununu", "--version"]), 0);
        let eval = Cli::try_parse_from([
            "mununu",
            "context",
            "eval",
            "m.ctxdsl",
            "--automaton",
            "A",
            "--fail-on",
            "none",
        ]);
        assert!(eval.is_ok(), "context eval takes --fail-on: {eval:?}");
    }

    #[test]
    fn the_0_5_verb_merges_parse_and_the_old_verbs_are_usage_errors() {
        let code = |argv: &[&str]| match Cli::try_parse_from(argv) {
            Ok(_) => 0,
            Err(e) => parse_error_exit_code(&e),
        };
        assert_eq!(
            code(&["mununu", "btor2", "verify", "d.btor2", "--engine", "cube"]),
            0
        );
        assert_eq!(
            code(&["mununu", "btor2", "verify", "d.btor2", "--engine", "ic3"]),
            0
        );
        assert_eq!(code(&["mununu", "btor2", "verify-safety", "d.btor2"]), 64);
        assert_eq!(code(&["mununu", "sv", "verify-bad", "m.sv"]), 0);
        assert_eq!(code(&["mununu", "sv", "verify", "m.sv"]), 64);
        // The liveness family is one verb.
        assert_eq!(
            code(&[
                "mununu",
                "btor2",
                "verify-liveness",
                "d",
                "--response",
                "a == 1 => b == 1"
            ]),
            0
        );
        assert_eq!(
            code(&[
                "mununu",
                "btor2",
                "verify-liveness",
                "d",
                "--request",
                "a == 1",
                "--grant",
                "b == 1",
                "--fairness",
                "c == 1"
            ]),
            0
        );
        assert_eq!(
            code(&[
                "mununu",
                "btor2",
                "verify-liveness",
                "d.aag",
                "--justice",
                "--rung",
                "exact"
            ]),
            0
        );
        assert_eq!(
            code(&[
                "mununu",
                "btor2",
                "verify-liveness",
                "d",
                "--justice",
                "--request",
                "a == 1"
            ]),
            64
        );
        assert_eq!(
            code(&["mununu", "btor2", "verify-liveness", "d", "--rung", "exact"]),
            64,
            "--rung needs --justice"
        );
        assert_eq!(
            code(&[
                "mununu",
                "btor2",
                "verify-liveness-all",
                "d",
                "--response",
                "a => b"
            ]),
            64
        );
        assert_eq!(code(&["mununu", "btor2", "verify-justice", "d"]), 64);
        assert_eq!(
            code(&[
                "mununu",
                "sv",
                "verify-liveness",
                "m.sv",
                "--request",
                "a == 1",
                "--grant",
                "b == 1",
                "--fairness",
                "c == 1"
            ]),
            0
        );
        assert_eq!(
            code(&[
                "mununu",
                "sv",
                "verify-liveness-all",
                "m.sv",
                "--response",
                "a => b"
            ]),
            64
        );
    }

    #[test]
    fn context_eval_fails_the_gate_on_a_refuted_formula_and_flags_sanity_findings() {
        assert_eq!(context_eval_outcome(Some(false), true), "violated");
        assert_eq!(context_eval_outcome(Some(false), false), "violated");
        assert_eq!(context_eval_outcome(Some(true), false), "unknown");
        assert_eq!(context_eval_outcome(None, false), "unknown");
        assert_eq!(context_eval_outcome(Some(true), true), "holds");
        assert_eq!(
            ci_exit_code(context_eval_outcome(Some(false), true), FailOn::Violated),
            2
        );
        assert_eq!(
            ci_exit_code(context_eval_outcome(None, false), FailOn::Violated),
            0
        );
        assert_eq!(
            ci_exit_code(context_eval_outcome(None, false), FailOn::Unknown),
            3
        );
    }

    // ---- mununu#537: expectation flag parsing --------------------------------------

    fn expect_args(
        all_hold: bool,
        count: Option<usize>,
        violated: &[&str],
        expect: &[&str],
    ) -> ExpectArgs {
        ExpectArgs {
            expect_all_hold: all_hold,
            expect_count: count,
            expect_violated: violated.iter().map(|s| s.to_string()).collect(),
            expect: expect.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_expect_flags_is_an_empty_claim() {
        // Which leaves the ordinary `--fail-on` gate in charge — the flags must not change
        // behaviour for anyone who does not use them.
        let exp = expect_args(false, None, &[], &[])
            .to_expectations()
            .expect("parses");
        assert!(exp.is_empty());
    }

    #[test]
    fn named_pairs_parse_case_insensitively() {
        use mununu_core::adapter::slang::expectations::ExpectedVerdict;
        let exp = expect_args(
            false,
            None,
            &[],
            &["a=HOLDS", "b=unknown", " c = Violated "],
        )
        .to_expectations()
        .expect("parses");
        assert_eq!(
            exp.named,
            vec![
                ("a".to_string(), ExpectedVerdict::Holds),
                ("b".to_string(), ExpectedVerdict::Unknown),
                ("c".to_string(), ExpectedVerdict::Violated),
            ]
        );
    }

    #[test]
    fn a_malformed_pair_is_a_usage_error_not_a_dropped_claim() {
        // A claim that silently does not run gates on NOTHING, which is worse than no claim at
        // all: the caller believes they are covered.
        let err = expect_args(false, None, &[], &["no_equals_sign"])
            .to_expectations()
            .expect_err("must reject");
        assert!(err.contains("NAME=VERDICT"), "got: {err}");

        let err = expect_args(false, None, &[], &["a=probably"])
            .to_expectations()
            .expect_err("must reject an unknown verdict");
        assert!(err.contains("not a verdict"), "got: {err}");

        let err = expect_args(false, None, &[], &["=holds"])
            .to_expectations()
            .expect_err("must reject an empty name");
        assert!(err.contains("name is empty"), "got: {err}");
    }

    #[test]
    fn the_expectation_exit_code_is_distinct_from_every_other() {
        // The whole point of #537's exit contract: "a verdict was not what you claimed" must be
        // tellable apart from "the run failed" (1) and from the ordinary gate (2 / 3).
        assert_eq!(EXPECTATIONS_UNMET_EXIT, 4);
        for verdict in ["holds", "violated", "unknown", "skipped"] {
            for fail_on in [FailOn::Violated, FailOn::Unknown, FailOn::None] {
                assert_ne!(
                    ci_exit_code(verdict, fail_on),
                    EXPECTATIONS_UNMET_EXIT,
                    "the verdict gate must never produce the expectation code"
                );
            }
        }
    }

    #[test]
    fn default_fails_on_violated_only() {
        assert_eq!(ci_exit_code("holds", FailOn::Violated), 0);
        assert_eq!(ci_exit_code("violated", FailOn::Violated), 2);
        assert_eq!(ci_exit_code("unknown", FailOn::Violated), 0);
        assert_eq!(ci_exit_code("skipped", FailOn::Violated), 0);
    }

    #[test]
    fn strict_also_fails_on_unknown() {
        assert_eq!(ci_exit_code("violated", FailOn::Unknown), 2);
        assert_eq!(ci_exit_code("unknown", FailOn::Unknown), 3);
        assert_eq!(ci_exit_code("holds", FailOn::Unknown), 0);
    }

    #[test]
    fn none_never_fails_on_the_verdict() {
        assert_eq!(ci_exit_code("violated", FailOn::None), 0);
        assert_eq!(ci_exit_code("unknown", FailOn::None), 0);
    }

    #[test]
    fn worst_ranks_violated_over_unknown_over_holds() {
        assert_eq!(worst_verdict(["holds", "holds"]), "holds");
        assert_eq!(worst_verdict(["holds", "unknown", "holds"]), "unknown");
        assert_eq!(worst_verdict(["unknown", "violated", "holds"]), "violated");
        // `skipped` (not evaluated) is not a failure.
        assert_eq!(worst_verdict(["skipped", "holds"]), "holds");
        assert_eq!(worst_verdict([] as [&str; 0]), "holds");
    }
}

/// mununu#543 W3 — args for the hidden `internal-engine-eval` verb.
#[derive(Args, Debug)]
struct InternalEngineEvalArgs {
    /// The lifted BTOR2 model (already cone-restricted, pinned and shadow-augmented).
    #[arg(long = "btor2", value_name = "FILE")]
    btor2: std::path::PathBuf,
    /// The μ-calculus formula to evaluate, as the engine would receive it.
    #[arg(long = "formula", value_name = "MU")]
    formula: String,
    /// Mirror of `sv verify-auto --no-antecedent-propagate` (mununu#629).
    #[arg(long = "no-antecedent-propagate")]
    no_antecedent_propagate: bool,
    /// Mirror of `sv verify-auto --no-input-state` (mununu#656).
    #[arg(long = "no-input-state")]
    no_input_state: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// mununu#543 W3 — INTERNAL. Evaluate one property with the exact-symbolic engine and print a
    /// one-line verdict, so the parent can run this engine in a CHILD PROCESS.
    ///
    /// Hidden because it is not a user surface: it exists so a stack overflow inside OxiDD's
    /// `apply_bin` becomes a child's wait status instead of killing the whole run. See
    /// `adapter::engine_isolate` for why in-process containment is impossible (a `stacker` red zone
    /// cannot fire inside the dependency, and a guard-page SIGSEGV aborts regardless of which
    /// thread it happens on).
    ///
    /// Takes the ARTIFACTS, not a recipe: a lifted BTOR2 file and the μ-calculus formula. The lift,
    /// cone restriction, config pins and shadow synthesis are already applied by the time this
    /// runs, so there is no CLI surface to reconstruct and no option struct to serialise — the
    /// child runs the identical code path on identical inputs.
    #[command(name = "internal-engine-eval", hide = true)]
    InternalEngineEval(InternalEngineEvalArgs),
    /// Inspect or manipulate Context DSL artefacts.
    Context {
        #[command(subcommand)]
        command: Box<ContextCommand>,
    },
    /// Extraction spec tools (validate, check provenance).
    Extraction {
        #[command(subcommand)]
        command: Box<ExtractionCommand>,
    },
    /// SystemVerilog analysis tools (discover significant values).
    Sv {
        #[command(subcommand)]
        command: Box<SvCommand>,
    },
    /// List available property templates.
    Templates(TemplatesArgs),
    /// Browse / emit shipped parameterised CTXDSL component templates
    /// (PLIC, watchdog, tracked-memory).
    Library {
        #[command(subcommand)]
        command: Box<LibraryCommand>,
    },
    /// Contract assume-guarantee tooling (validate discharge graphs, etc.).
    Contract {
        #[command(subcommand)]
        command: Box<ContractCommand>,
    },
    /// HW/SW codesign tools — register-map sidecars + coupling synthesis.
    Codesign {
        #[command(subcommand)]
        command: Box<CodesignCommand>,
    },
    /// Run the general N-source verification framework against a
    /// `verify.toml` project config. Each source is dispatched
    /// through its adapter, alphabet bindings are applied,
    /// composition is realised, and every declared property is
    /// evaluated. See `crates/mununu-core/src/verify/` for the
    /// pipeline.
    Verify(VerifyArgs),
    /// Memory-soundness tooling — surface declared
    /// `[sources.memory_abstraction]` postures and flag property
    /// formulas that reference memory in ways the declared posture
    /// cannot soundly support. Advisory by default; `--strict`
    /// converts warnings into a non-zero exit.
    Memory {
        #[command(subcommand)]
        command: Box<MemoryCommand>,
    },
    /// Start HTTP API server
    Server {
        /// Server address (default: 127.0.0.1:8080)
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        /// The deployment configuration this server exposes (`mununu.toml`); same as the global
        /// `--deployment-config`.
        #[arg(long = "config", value_name = "FILE")]
        config: Option<PathBuf>,
    },
    /// BTOR2-direct analysis tools (Phase A.4 predicate-image discovery).
    Btor2 {
        #[command(subcommand)]
        command: Box<Btor2Command>,
    },
}

#[derive(Subcommand, Debug)]
enum Btor2Command {
    /// Run SMT predicate-image discovery on a BTOR2 file.
    ///
    /// For each user-named state cell, enumerate the values reachable
    /// in one transition step under any current state + input
    /// combination. Writes the resulting `discovered_values` map to
    /// the path-adjacent `<stem>.mununu.json` (or `--output` if
    /// provided), so the existing BTOR2 bit-blast / sidecar resolver
    /// pipeline picks the abstraction up without further wiring.
    ///
    /// See `docs/design/auto-extraction-architecture.md` §2 Stage 4
    /// for the soundness contract.
    Discover(Btor2DiscoverArgs),
    /// Lift a BTOR2 source into the KMTS-aware shape (R.2).
    ///
    /// Runs the existing 2-valued BTOR2 adapter and post-hoc
    /// enriches the output with per-state 3-valued AP labellings
    /// derived from the bit-blaster's `state_valuations`. Prints a
    /// summary JSON: number of predicates synthesised, total
    /// `(state, predicate)` labellings, per-automaton state count.
    /// Used by the R.2 fixture sweep to validate the lifter shape
    /// stays consistent across runs.
    LiftKmts(Btor2LiftKmtsArgs),
    /// R.5 Item 3 sub-item 3.5 (2026-06-04) — Run the R.5 CEGAR
    /// refinement loop on a BTOR2 fixture + μ-calculus formula.
    ///
    /// Selects a predicate-discovery source (`wp` heuristic by
    /// default, or `craig` for Craig interpolation via the
    /// CVC5 subprocess). When `craig` is selected but the CVC5
    /// binary isn't found at runtime, the loop emits a structured
    /// warning + falls back to the `wp` heuristic automatically.
    /// See `docs/external-tools.md` for CVC5 install instructions.
    Cegar(Btor2CegarArgs),
    /// Decide `bad`-reachability with the multi-engine safety portfolio.
    ///
    /// Runs every available sound engine over the BTOR2 design and merges
    /// them under the differential-oracle discipline: the exact BDD engine,
    /// the in-house native (BMC + k-induction) and SPACER (IC3/PDR +
    /// interpolation) engines — all in-process — plus the `btormc` and
    /// `Pono` subprocess members when their binaries are on PATH. Prints the
    /// merged verdict (`reachable` / `unreachable` / `unknown` /
    /// `contradiction`) and which engines reached each conclusion. A
    /// `contradiction` means two sound engines disagree — a soundness alarm,
    /// never a silent guess. Engines whose binary is absent simply abstain.
    Verify(Btor2VerifyArgs),
    /// Apply the fault-injection rewrite to a BTOR2 design and print the result: for each
    /// `--fault REG[:k]` the register's `next` becomes `ite(fire, mununu_fault_val_i, next)`
    /// with a named enable input, a global budget counter `mununu_fault_cnt` and a warm-up
    /// clock. What `sv verify-auto --fault` and `btor2 verify --fault` run on, as a file —
    /// for reproducing a verdict with another engine.
    ///
    /// surface: CLI-only — a file transform; the posture itself is CLI+API on the verify verbs.
    InjectFault(Btor2InjectFaultArgs),
    /// Internal: run z3-SPACER on a BTOR2 read from stdin, print `safe`/`unsafe`/
    /// `unknown`. The portfolio self-execs this to run SPACER in an isolated child
    /// process (z3's Fixedpoint can flaky-segfault); not a user-facing verb.
    #[command(hide = true)]
    SpacerCheck(Btor2SpacerCheckArgs),
    /// Decide liveness: the one liveness verb (0.5 merges `-all`, `-under-fairness` and
    /// `verify-justice` into it).
    ///
    /// - `--request A --grant G`: the response property `AG(A → AF G)`, reduced to one
    ///   `bad`-reachability query (Biere–Artho–Schuppan liveness-to-safety) the portfolio decides.
    /// - repeatable `--response "A => G"`: the conjunction `⋀ᵢ AG(aᵢ → AF gᵢ)`, one query per
    ///   conjunct (`violated` if any conjunct is, else `unknown` if any is, else `holds`).
    /// - repeatable `--fairness F` (with `--request` / `--grant`): the same property under the
    ///   fairness assumptions `⋀ⱼ GF fⱼ` (the Emerson–Lei fair-cycle l2s).
    /// - `--justice`: HWMCC-style justice properties — every `justice` line of the design (BTOR2,
    ///   or AIGER 1.9 converted first), decided by the exact fair-cycle fixpoint and/or l2s
    ///   (`--rung`, `--timeout-ms`, `--parallel-rungs`).
    ///
    /// Atoms are single register comparisons (`"st == 1"`). Surface peer of
    /// `POST /api/v1/btor2/verify-liveness`.
    VerifyLiveness(Btor2VerifyLivenessArgs),
    /// Convert an AIGER 1.9 file (`.aig` binary or `.aag` ASCII; bad / constraint / justice /
    /// fairness) to BTOR2 text.
    FromAiger(Btor2FromAigerArgs),
    /// Decide recoverability `AG EF good` — the branching property SVA cannot state.
    ///
    /// "From every reachable state, can the design still get back to a `good`
    /// state?" A `violated` verdict means a reachable state is a trap from which
    /// `good` is unreachable. Decided by the exact 3-valued engine (sound at every
    /// alternation depth, definite within its bit cap — 40 by default, auto-raised to 192; `unknown` over the cap —
    /// use `btor2 cegar … --must-edge-inference smt-hyper-must` for wider designs).
    /// `--target` is a single register-comparison atom (`"state_q == 3"`).
    VerifyRecoverability(Btor2VerifyRecoverabilityArgs),
    /// Auto-scan every FSM-like state register for a reachable illegal encoding — no input.
    ///
    /// For each narrow (≤ `--max-width` bit) state register, derive its legal encodings
    /// from the design (the constants its own logic compares it against, plus its reset
    /// value) and check — from the real reset state — whether any value **outside** that
    /// set is reachable. A `violated` register has a reachable **illegal encoding**: some
    /// input drives the FSM past its enum (an incomplete `case`, a missing `default`), an
    /// unambiguous bug. A `holds` register provably stays within its encoding. Decided by
    /// the word-level reachability portfolio (scales past the exact engine). Prints one
    /// line per register; exits non-zero on any reachable illegal encoding.
    CheckFsm(Btor2CheckFsmArgs),
    /// Solve a two-player controllable-reachability GAME and synthesize the winner's strategy.
    ///
    /// Partitions the primary inputs into controller-owned (`--controllable`, repeatable) vs
    /// environment-owned (the rest, adversarial), then decides whether the CONTROLLER can force the
    /// design to the `--good` state atom against every environment move (the Mealy game
    /// `μX. good ∨ ⟨ctrl⟩X`). Prints `realizable` + the controller's Mealy strategy when it wins, or
    /// `unrealizable` + the environment's positional COUNTERSTRATEGY (the witness for why no controller
    /// works — e.g. an ack the environment withholds, motivating an assume-guarantee assumption) when it
    /// does not. Decided by the exact-symbolic ROBDD engine (definite within its cap). Surface peer of
    /// `POST /api/v1/btor2/game`. Exits non-zero (`--fail-on`) on `unrealizable`.
    Game(Btor2GameArgs),
}

/// R.5 Item 3 sub-item 3.5 (2026-06-04) — predicate-source
/// selector for `mununu btor2 cegar`. Mirrors the values of
/// [`mununu_core::adapter::btor2::cegar::PredicateSource`] but
/// omits the `Manual` variant (callback-only; not selectable
/// from the CLI).
#[derive(Clone, Debug, Copy, clap::ValueEnum)]
enum PredicateSourceArg {
    /// Weakest-precondition heuristic (default). No external deps.
    Wp,
    /// Craig interpolation via CVC5 subprocess. Requires CVC5 ≥ 1.0
    /// installed locally; falls back to `wp` with a warning when
    /// the binary is absent.
    Craig,
}

/// DR1 (IR-unification track, 2026-06-19) — may-edge inference
/// selector for `mununu btor2 cegar`. Mirrors
/// [`mununu_core::adapter::btor2::kmts_lift::MayEdgeInference`].
#[derive(Clone, Debug, Copy, clap::ValueEnum, Default)]
enum MayEdgeInferenceArg {
    /// Sampling-based may-edges. Fast, but an under-approximation of the
    /// may relation — sampling can MISS a real may-edge, which is unsound
    /// for safety. Preserves the pre-DR1 CEGAR behaviour; opt in when the
    /// input space is small enough to enumerate exhaustively.
    Off,
    /// Sound all-pairs SMT may-edges (default, AR-S2): for every (src, tgt)
    /// cube pair Z3 decides whether a concrete witness exists; an edge is
    /// excluded only when proven impossible (a sound over-approximation).
    /// O(cubes²) SMT calls — tractable at small cube counts. Combining with
    /// a non-`off` `--must-edge-inference` is not yet wired.
    #[default]
    SmtAllPairs,
}

/// Safety engine selector for `mununu btor2 verify --engine`.
///
/// surface: CLI-only — the `ic3` engine (IC3ia predicate abstraction,
/// [`mununu_core::adapter::btor2::abs_safety::verify_safety_ic3`]) is an experimental,
/// guarded foundation: on real designs its backward refinement stalls and it abstains
/// (the P2.3b make-or-break finding). It is surfaced for evaluation, NOT production
/// routing — the production safety path is `cube`. No API/UI surface until it decides
/// real cases competitively with the cube + `native_interp`.
#[derive(Clone, Debug, Copy, clap::ValueEnum, Default)]
enum SafetyEngineArg {
    /// The bit-level reachability portfolio (default): exact ⊕ native BMC / k-induction ⊕
    /// interpolation ⊕ the subprocess model checkers.
    #[default]
    Portfolio,
    /// The KMTS 3-valued cube — [`verify_safety_scalable`]. Enumeration + emergent-K
    /// interpolation discovery: the branching-cube route on safety (formerly `verify-safety`).
    ///
    /// [`verify_safety_scalable`]: mununu_core::adapter::recoverability::verify_safety_scalable
    Cube,
    /// Experimental IC3ia predicate-abstraction frame ladder — `verify_safety_ic3`.
    /// Guarded foundation; abstains where refinement stalls. Uses cvc5 for refinement
    /// (abstains when cvc5 is absent).
    Ic3,
}

/// R-F5.4.2b (2026-07-03) — which predicate-cube engine evaluates the
/// property. Mirrors the two edge-construction strategies.
#[derive(Clone, Debug, Copy, clap::ValueEnum, Default, PartialEq, Eq)]
enum EngineArg {
    /// Explicit predicate-cube lift + CEGAR refinement loop (default): the
    /// may/must edges are built with SMT (`O(2^2|P|)` at `--may/must-edge-inference`
    /// = smt), which is the scaling wall at large `|P|`. Supports the full
    /// refinement loop, `--predicate-source`, sidecar compound predicates, etc.
    #[default]
    Explicit,
    /// R-F5 symbolic engine: build the may/must transition relation as BDDs
    /// directly from the BTOR2 (no per-cube-pair SMT) and evaluate the formula
    /// by BDD image/preimage + μ/ν fixpoint. Orders of magnitude faster at large
    /// `|P|` (≈10 ms at `|P|=12` where the explicit SMT path takes ~29 min).
    /// Runs the CEGAR refinement loop (WP predicate discovery on ⊥, rebuilding
    /// the relation each iteration; `--max-iterations 0` = single-shot).
    ///
    /// **Scope:** simple `--predicate [NAME:]REG == VALUE` equalities + non-derived
    /// `--sidecar` `compound_predicates` (`cnt >= 2`, relational); the bare
    /// `[]`/`<>` fragment only (guarded / controllability / step-bounded
    /// modalities error). Derived/combinational (per-cube SMT label) predicates
    /// and the Clts-only optimisations (failure-subgame precision, approximant
    /// reuse, CTXDSL emit) are still `--engine explicit` only.
    Symbolic,
    /// D1 exact symbolic MC: decide the μ-calculus EXACTLY over the full
    /// bit-blasted state (no predicate abstraction), by ROBDD μ/ν fixpoint from the
    /// reset init. The verdict is **definite** (2-valued Holds/Violated, never ⊥) —
    /// it decides `AF`-liveness (and any μ-calculus property) where the abstraction
    /// engines return Unknown: over the exact finite state the fixpoint IS the
    /// ranking, so no ranking/fairness infra is needed. Bounded by BDD size (a
    /// design too large to bit-blast ⇒ the property is `Skipped`). Verify-auto only
    /// (the surface that supplies a reset-gated model).
    ExactSymbolic,
    /// PORTFOLIO (SEQUENTIAL) — run the engines in precision order (exact → symbolic
    /// → explicit), stopping as soon as every property is decided. Takes the definite
    /// verdict from whichever engine produces one; a ⊥ means all engines left it
    /// undecided. Proven sound (the engines never contradict). The budget-FRUGAL
    /// choice: often only the exact engine runs. Verify-auto only.
    PortfolioSequential,
    /// PORTFOLIO (PARALLEL) — run ALL engines concurrently and merge; each property
    /// takes the definite verdict from whichever engine decided it. Same verdicts as
    /// `portfolio-sequential` but minimum latency at 3× compute — the budget-RICH,
    /// low-latency choice. Verify-auto only.
    PortfolioParallel,
}

/// R.2.5b session-1 follow-up (2026-06-06) — must-edge inference
/// selector for `mununu btor2 cegar`. Mirrors the values of
/// [`mununu_core::adapter::btor2::kmts_lift::MustEdgeInference`].
/// RTL front-end for the SV → BTOR2 lift (CLI mirror of
/// How `sv verify-auto` treats SVA `assume` statements
/// ([`mununu_core::adapter::slang::verify_auto::AssumeMode`]).
#[derive(Clone, Debug, Copy, clap::ValueEnum, Default)]
enum AssumesArg {
    /// Apply each supported assumption as a precondition: the guarantees are checked on the
    /// traces along which every assumption has held so far.
    #[default]
    Precondition,
    /// Check each assumption like an assertion; it constrains nothing.
    Check,
    /// Drop the assumptions: neither checked nor applied.
    Ignore,
}

/// [`mununu_core::adapter::yosys::SvFrontend`]).
#[derive(Clone, Debug, Copy, clap::ValueEnum, Default)]
enum SvFrontendArg {
    /// Env-driven default: `MUNUNU_YOSYS_FRONTEND=slang` → slang, else read_verilog.
    #[default]
    Auto,
    /// Force yosys `read_verilog`.
    Verilog,
    /// Force the yosys-slang plugin (`read_slang`) — lifts modern-SV constructs
    /// `read_verilog`/sv2v reject (`while` loops, `module M import pkg::*;`).
    /// Requires the yosys-slang plugin (present in the `mununu-sva` image).
    Slang,
    /// Run sv2v first, then yosys `read_verilog` (replaces `--frontend sv2v`, 0.5).
    Sv2v,
}

impl SvFrontendArg {
    /// Whether the lift runs sv2v before yosys.
    fn sv2v(self) -> bool {
        matches!(self, SvFrontendArg::Sv2v)
    }
}

impl From<SvFrontendArg> for mununu_core::adapter::yosys::SvFrontend {
    fn from(a: SvFrontendArg) -> Self {
        match a {
            SvFrontendArg::Auto => Self::Auto,
            SvFrontendArg::Verilog | SvFrontendArg::Sv2v => Self::Verilog,
            SvFrontendArg::Slang => Self::Slang,
        }
    }
}

#[derive(Clone, Debug, Copy, clap::ValueEnum, Default)]
enum MustEdgeInferenceArg {
    /// mununu#663 (default) — `sv verify-auto` resolves the policy PER PROPERTY:
    /// a Liveness/Mixed-class formula (a μ under a ν — `AG EF`, `AG AF`, the
    /// response shapes, whose 3-valued `◇` turns true only through MUST edges)
    /// runs its cube member with `smt-per-target-standard`; safety / reachability /
    /// propositional formulas stay may-only. On the other verbs `auto` is `off`.
    #[default]
    Auto,
    /// Pre-R.2.5b behaviour. Only MayOnly edges emitted; no must / hyper-must
    /// inference — also switches the `auto` resolution OFF in `sv verify-auto`.
    Off,
    /// R.2.5b session 2 (2026-06-08). SMT-backed must-edge inference
    /// via Z3 BV theory using the **stronger ∀∀ form** (`∀ state.
    /// ∀ input. transition ⟹ next ⊨ tgt` — deterministic into tgt
    /// regardless of input). SOUNDNESS: SMT-proved (no sampling).
    /// Verdicts carry an `[R.2.5b-smt-must]` AdapterWarning.
    SmtPerTarget,
    /// R.2.5b session-2 follow-up (2026-06-09). SMT-backed must-edge
    /// inference using the **canonical ∀∃ form** (`∀ state ⊨ src.
    /// ∃ input. next ⊨ tgt`) via Z3 quantifier alternation. Strictly
    /// more permissive than `smt-per-target` (every ∀∀-Must is also
    /// ∀∃-Must; the ∀∃ form additionally promotes edges where SOME
    /// input per state reaches tgt). Verdicts carry an
    /// `[R.2.5b-smt-must-standard]` AdapterWarning.
    SmtPerTargetStandard,
    /// R.2.5b session-2 follow-up (2026-06-09). SMT-backed
    /// **hyper-must** inference. For each source cube, tries the
    /// per-target ∀∃ singleton checks first; if no singleton proves
    /// Must, runs a full-target-set ∀∃ check and emits
    /// `MustHyperOnly` with the full sampled target set on Must.
    /// Verdicts carry an `[R.2.5b-smt-must-hyper]` AdapterWarning.
    SmtHyperMust,
}

#[derive(Args, Debug)]
struct Btor2CegarArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// μ-calculus formula to evaluate over the lifted KMTS.
    /// Example: `'nu X. < step > X'` for a simple liveness
    /// formula. Wrap in single quotes to avoid shell expansion.
    #[arg(long, value_name = "FORMULA")]
    formula: String,
    /// Initial predicate set, `[NAME:]REG == VALUE` (the name defaults to the expression).
    /// May be repeated. At least one is required to bootstrap
    /// the cube space.
    /// Example: `--predicate "p_idle:state == 0" --predicate "state == 1"`.
    #[arg(long = "predicate", value_name = "[NAME:]REG == VALUE")]
    predicates: Vec<String>,
    /// Predicate-discovery source for the CEGAR loop.
    #[arg(long, value_enum, default_value_t = PredicateSourceArg::Wp)]
    predicate_source: PredicateSourceArg,
    /// Explicit CVC5 binary path (overrides `MUNUNU_CVC5_PATH`
    /// env var + `$PATH` discovery). Only consulted when
    /// `--predicate-source craig`.
    #[arg(long, value_name = "PATH")]
    cvc5_path: Option<PathBuf>,
    /// Maximum number of CEGAR refinement iterations before
    /// terminating with `BoundedIterationsReached`. Default 16.
    #[arg(long, default_value_t = 16)]
    max_iterations: usize,
    /// R.2.5b session-1 follow-up (2026-06-06) — must-edge
    /// inference policy for the per-iteration `predicate_cube_lift`.
    /// Defaults to `off`. Set to `sampling-confluence` to opt the
    /// lifter into sampling-derived Sharp / MustHyperOnly edge
    /// emission. The lift result then includes an
    /// `[R.2.5b-sampling-must]` AdapterWarning whenever the
    /// post-pass fires.
    #[arg(long, value_enum, default_value_t = MustEdgeInferenceArg::Off)]
    must_edge_inference: MustEdgeInferenceArg,
    /// DR1 (IR-unification track, 2026-06-19) — may-edge inference
    /// policy for the per-iteration `predicate_cube_lift`. Defaults to
    /// `off` (sampling may-edges; fast but an under-approximation of the
    /// may relation, unsound for safety). Set to `smt-all-pairs` for the
    /// sound all-pairs SMT may relation (an edge is excluded only when
    /// Z3 proves it impossible).
    #[arg(long, value_enum, default_value_t = MayEdgeInferenceArg::SmtAllPairs)]
    may_edge_inference: MayEdgeInferenceArg,
    /// R-S8 session 2 (2026-06-08) — under-constrained constant's
    /// admissible value set. Format: `REGISTER=v1,v2,v3`. May be
    /// repeated. Bridges to the R-Y7 symbolic-init path by
    /// expanding the predicate-cube lifter's initial-state set
    /// to all cubes admissible under the listed values.
    /// Example: `--init-values 'boot_fsm=0,1,2'
    /// --init-values 'mode=0,1'`.
    ///
    /// When a sidecar declares `signals[].config_values`, the
    /// sidecar value takes precedence for that register (the CLI
    /// flag's values are merged in; sidecar values override CLI
    /// values per the resolver's last-write-wins for the same
    /// key).
    #[arg(long = "init-values", value_name = "REG=v1,v2,...")]
    init_values: Vec<String>,
    /// Emit the trace summary as JSON on stdout instead of the
    /// human-readable format.
    #[arg(long)]
    json: bool,
    /// Cube-CEGAR engine. **Default `explicit`** (SMT edges + refinement loop);
    /// `symbolic` uses the R-F5 BDD relation (no per-cube-pair SMT). The
    /// `exact-symbolic` and `portfolio-*` engines are `sv verify-auto`-only (they need
    /// the reset-gated model verify-auto builds), so `btor2 cegar` rejects them.
    #[arg(long, value_enum, default_value_t = EngineArg::Explicit)]
    engine: EngineArg,
    /// R.6.6 / V.6 (2026-06-09) — name of a BTOR2 input symbol the
    /// controller drives. Repeated to declare multiple controllable
    /// inputs. When non-empty, the predicate-cube lifter partitions
    /// boolean inputs into env (uncontrollable) + ctrl (controllable)
    /// classes per-symbol-name + emits per-combo dual-label
    /// transitions `[env_cN, ctrl_cM]` with appropriate
    /// `LabelControllability` tags. The Skolem grouping in the
    /// R.6.3 modal step then partitions correctly along the
    /// controllability axis (∀ env-combo, ∃ ctrl-combo for the
    /// synthesis idiom).
    ///
    /// When omitted (default empty): the lifter emits the legacy
    /// single-`step` label shape (pre-R.6.6 behaviour, no
    /// controllability axis on labels).
    ///
    /// Example: `--controllable ctrl_g0 --controllable ctrl_g1`.
    #[arg(long = "controllable", value_name = "INPUT")]
    controllable_inputs: Vec<String>,
    /// R-S2b.6 (§Phase 9 §9.1, 2026-06-12) — path to the original
    /// SystemVerilog source for the BTOR2 input. When set AND
    /// the sidecar declares a `simulate_reset` block AND a
    /// Verilator binary is discoverable, the BTOR2 bit-blaster
    /// runs a short concrete reset simulation through Verilator
    /// and feeds the post-reset register valuations into the
    /// EnumValues discriminator lists (§Phase 9 R-S2b strategy).
    /// Verilator-absent / sidecar-omits-simulate_reset / SV-path-
    /// missing all fall through silently with an explanatory
    /// AdapterWarning — the feature is opt-in per sidecar.
    ///
    /// Example: `--sv-source designs/uart_tx.sv`.
    #[arg(long = "sv-source", value_name = "PATH")]
    sv_source: Option<PathBuf>,
    /// R-S6.6 (§Phase 9 §9.1, 2026-06-12) — path to a sidecar
    /// JSON file (`.mununu.json`). When set, the file's contents
    /// override the synthetic sidecar built from `--init-values`,
    /// AND `AdapterOptions::sidecar_path` is populated so the
    /// bit-blaster's `apply_vcd_trace_seeding` orchestration can
    /// resolve relative `vcd_traces` paths against the sidecar's
    /// parent directory.
    ///
    /// Without `--sidecar`, only absolute VCD trace paths in a
    /// `--init-values`-built sidecar can be read; relative
    /// paths emit an `AdapterWarning` and fall through.
    ///
    /// Example: `--sidecar examples/v6_controllability_kmts/source/amba_arbiter.mununu.json`.
    #[arg(long = "sidecar", value_name = "PATH")]
    sidecar: Option<PathBuf>,
    /// CTXDSL Phase 2 (2026-06-22) — opt-in: write the final refined
    /// predicate-cube model + the checked formula to this path as a
    /// self-contained CTXDSL document. Default off (the cube is dropped at
    /// loop exit). The emitted CTXDSL carries the cube states' 3-valued
    /// (`predicates_3v`) labels + transition modality + a `mu_formulas`
    /// block with the formula. Pass `/dev/stdout` to print to the terminal.
    ///
    /// Example: `--emit-ctxdsl /tmp/cegar_model.ctxdsl`.
    #[arg(long = "emit-ctxdsl", value_name = "PATH")]
    emit_ctxdsl: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct Btor2DiscoverArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// Where to write the updated sidecar. Defaults to
    /// `<stem>.mununu.json` next to the BTOR2 input.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
    /// Maximum number of distinct values to enumerate per state cell.
    /// Default matches `ImageOptions::default().cap_edges = 4096`.
    #[arg(long, default_value_t = 4096)]
    cap_edges: usize,
}

#[derive(Args, Debug)]
struct Btor2LiftKmtsArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// Cap on the number of predicates the lifter synthesises.
    /// Each `(register, value)` pair in the bit-blaster's state
    /// valuations becomes one predicate; `None` (the default)
    /// means no cap.
    #[arg(long, value_name = "N")]
    max_predicates: Option<usize>,
}

/// Arguments for `mununu btor2 verify` — the multi-engine safety portfolio.
/// Internal `btor2 spacer-check` args — BTOR2 is read from stdin.
#[derive(Args, Debug)]
struct Btor2SpacerCheckArgs {
    /// z3-SPACER wall-clock timeout in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    timeout_ms: u32,
}

#[derive(Args, Debug)]
struct Btor2VerifyArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// Decide with ONLY the mununu-owned engines (exact BDD, native BMC /
    /// k-induction, native McMillan interpolation, and the deep counterexample
    /// search) — no external SPACER / btormc / Pono. Answers "what can mununu's own
    /// algorithms decide on their own?", for no-subprocess deployments and the
    /// soundness cross-check.
    ///
    /// surface: CLI-only — a soundness-audit / no-subprocess diagnostic, peer of the
    /// CLI-only `btor2 verify --engine ic3`; the default full portfolio remains the
    /// CLI+API+UI path.
    #[arg(long)]
    owned_only: bool,
    /// Safety engine: `portfolio` (default), `cube` (the KMTS 3-valued predicate cube, the
    /// branching-cube route; a `violated` is downgraded to `unknown` when the design has
    /// `constraint` lines) or `ic3` (experimental IC3ia, evaluation only). The single-engine
    /// routes take none of `--fault`, `--target`, `--witness`, `--owned-only`.
    #[arg(long, value_enum, default_value_t = SafetyEngineArg::Portfolio)]
    engine: SafetyEngineArg,
    /// Per-engine wall budget (ms), default 60000: the subprocess members (btormc / Pono) of
    /// the full portfolio, or every engine under `--owned-only`. A larger value lets the
    /// incremental-SAT model checkers and native interpolation reach deeper — e.g. `krebs.3`'s
    /// depth-75 counterexample needs ~73000. (0.5: replaces `--owned-timeout-ms` too.)
    #[arg(long, value_name = "MS")]
    timeout_ms: Option<u64>,
    /// On a `violated` verdict, also emit a concrete init→bad counterexample trace
    /// (per-cycle state + input assignments) in the summary JSON. Re-derives the
    /// SHALLOWEST witness via the native bit-precise BMC engine (bounded to
    /// `--witness-max-k` cycles); omitted if that bound doesn't reach the `bad` node.
    /// This is the actionable payload for an LLM RTL-refinement loop — verdict + the
    /// exact stimulus that trips the assertion.
    ///
    /// surface: CLI-only — a diagnostic augmentation of the CLI-only `btor2 verify`;
    /// the default portfolio verdict remains the CLI+API+UI path.
    #[arg(long)]
    witness: bool,
    /// Cycle bound for `--witness` counterexample re-derivation (default 200).
    #[arg(long, value_name = "K", default_value_t = 200)]
    witness_max_k: u32,
    /// BMC-only bounded reachability: run ONLY the native bit-precise BMC to `--bmc-k`
    /// steps and skip the full portfolio (no equivalence/safety PROOF). A `bad` reached
    /// within the bound ⇒ `violated` (a sound, shallow counterexample); no `bad` within
    /// the bound ⇒ `unknown` (BOUNDED — not a safety proof, a deeper CEX may exist). Turns
    /// the wide-datapath equivalence-miter timeouts (where the proof is intractable but a
    /// distinguishing input is shallow) into fast definite `violated` decisions.
    ///
    /// surface: CLI-only — a bounded-reachability diagnostic, peer of `--owned-only`; the
    /// default full portfolio remains the CLI+API+UI verdict path.
    #[arg(long)]
    bmc_only: bool,
    /// Step bound for `--bmc-only` (default 20).
    #[arg(long, value_name = "K", default_value_t = 20)]
    bmc_k: u32,
    /// Fault-injection posture on the BTOR2 (`REG[:k]`, repeatable; the one global budget,
    /// default 1): the design is rewritten as `btor2 inject-fault` does before the portfolio
    /// runs. `reachable` then reads "an attack within the budget reaches `bad`" (the attacker
    /// also drives every input — the strongest attacker); `unreachable` is a proof for every
    /// input behaviour and every schedule within the budget. Mirrored on the API as `faults`.
    #[arg(long = "fault", value_name = "REG[:k]")]
    fault: Vec<String>,
    /// Cycles before the first injection may fire (default 10). Mirrored on the API as
    /// `fault_warmup`.
    #[arg(long = "fault-warmup", value_name = "CYCLES", default_value_t = 10)]
    fault_warmup: u32,
    /// The k-fault adversary's goal (with `--fault`): a boolean atom (`state_q == 680`,
    /// `a == 1 && b == 3`) the attacker tries to reach instead of the design's own `bad` lines.
    /// `violated` = an attack within the budget reaches it (the attacker also drives every
    /// input — the strongest attacker; the JSON lists the injections under `attack`), `holds` =
    /// no schedule of at most `k` injections after the warm-up does. Mirrored on the API as
    /// `target`.
    #[arg(long = "target", value_name = "ATOM", requires = "fault")]
    target: Option<String>,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu btor2 inject-fault` — the fault-injection rewrite as a file transform.
#[derive(Args, Debug)]
struct Btor2InjectFaultArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// A register the adversary may overwrite (`REG[:k]`, repeatable; one global budget,
    /// default 1; `REG:unbounded` lifts it).
    #[arg(long = "fault", value_name = "REG[:k]", required = true)]
    fault: Vec<String>,
    /// Cycles before the first injection may fire (default 10).
    #[arg(long = "fault-warmup", value_name = "CYCLES", default_value_t = 10)]
    fault_warmup: u32,
    /// Write the injected BTOR2 here instead of standard output.
    #[arg(short = 'o', long = "output", value_name = "PATH")]
    output: Option<PathBuf>,
}

/// Arguments for `mununu btor2 verify-liveness` — the response-liveness reduction.
/// Arguments for `mununu btor2 verify-liveness` — the merged liveness verb.
#[derive(Args, Debug)]
struct Btor2VerifyLivenessArgs {
    /// Path to the BTOR2 input (or, with `--justice`, an AIGER 1.9 file).
    #[arg(value_name = "FILE")]
    file: PathBuf,
    /// The request atom of `AG(request → AF grant)`.
    #[arg(long, value_name = "ATOM", requires = "grant", conflicts_with_all = ["responses", "justice"])]
    request: Option<String>,
    /// The grant atom that must eventually follow on every path.
    #[arg(long, value_name = "ATOM", requires = "request")]
    grant: Option<String>,
    /// A response pair `"ANTE => CONS"` of the conjunction (repeatable).
    #[arg(long = "response", value_name = "ANTE => CONS", conflicts_with_all = ["fairness", "justice"])]
    responses: Vec<String>,
    /// A fairness atom assumed to hold infinitely often (repeatable; with `--request` / `--grant`).
    #[arg(long = "fairness", value_name = "ATOM", requires = "request")]
    fairness: Vec<String>,
    /// Decide the design's own `justice` lines (HWMCC semantics) instead of a response property.
    #[arg(long, conflicts_with_all = ["request", "responses"])]
    justice: bool,
    /// `--justice`: `exact`, `l2s` or `both` (the default: the exact ladder for 30 s, then l2s,
    /// then the exact ladder with the remainder).
    #[arg(long, default_value = "both", requires = "justice")]
    rung: String,
    /// `--justice`: wall-clock budget per justice set in milliseconds.
    #[arg(long, value_name = "MS", requires = "justice")]
    timeout_ms: Option<u64>,
    /// `--justice` with `--rung both`: run both rungs concurrently, first definite verdict wins.
    #[arg(long, requires = "justice")]
    parallel_rungs: bool,
    /// `--justice`: emit the per-set report as JSON only.
    #[arg(long, requires = "justice")]
    json: bool,
    #[command(flatten)]
    ci: CiArgs,
}

#[derive(Args, Debug)]
struct Btor2VerifyJusticeArgs {
    /// BTOR2 file, or an AIGER 1.9 file (`.aig` binary / `.aag`) converted on the fly.
    #[arg(value_name = "FILE")]
    file: PathBuf,
    /// `exact` (ROBDD fair-cycle fixpoint), `l2s` (liveness-to-safety + the reachability
    /// portfolio) or `both` (the default: the exact ladder for 30 s, then l2s, then the exact
    /// ladder with the remainder).
    #[arg(long, default_value = "both")]
    rung: String,
    /// Wall-clock budget per justice set in milliseconds (default: the engines' own budgets).
    #[arg(long, value_name = "MS")]
    timeout_ms: Option<u64>,
    /// Under `--rung both`, run the exact ladder and the l2s rung concurrently; the first
    /// definite verdict wins and the other is cancelled.
    #[arg(long)]
    parallel_rungs: bool,
    /// Emit the per-justice-set report as JSON only.
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    ci: CiArgs,
}

#[derive(Parser, Debug)]
struct Btor2FromAigerArgs {
    /// AIGER 1.9 input (`.aig` binary or `.aag` ASCII).
    #[arg(value_name = "AIGER_FILE")]
    file: PathBuf,
    /// Output BTOR2 path (stdout when omitted).
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct Btor2LivenessOneArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// The request atom — a register comparison, e.g. `"st == 1"`.
    #[arg(long, value_name = "ATOM")]
    request: String,
    /// The grant atom that must eventually follow on every path, e.g. `"st == 2"`.
    #[arg(long, value_name = "ATOM")]
    grant: String,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu btor2 verify-liveness --response` — a conjunction of
/// response-liveness properties, each a repeatable `--response "ANTE => CONS"`.
#[derive(Args, Debug)]
struct Btor2VerifyLivenessAllArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// A response pair `"ANTE => CONS"` — both sides register-comparison atoms
    /// (`"req == 1 => grant == 1"`). Repeatable; the verdict is the conjunction
    /// `⋀ AG(ANTE → AF CONS)`. At least one required.
    #[arg(long = "response", value_name = "ANTE => CONS", required = true)]
    responses: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu btor2 verify-liveness --fairness` — the fair-cycle
/// l2s (Emerson–Lei) extension of `verify-liveness` (mununu#477 Option B).
#[derive(Args, Debug)]
struct Btor2VerifyLivenessUnderFairnessArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// The request atom — a register comparison, e.g. `"req == 1"`.
    #[arg(long, value_name = "ATOM")]
    request: String,
    /// The grant atom that must eventually follow on every path, e.g. `"ack == 1"`.
    #[arg(long, value_name = "ATOM")]
    grant: String,
    /// A fairness atom `GF <atom>` — a single register-comparison atom whose
    /// satisfaction is assumed to hold infinitely often on every path considered.
    /// Repeatable; the verdict is the response under the conjunction
    /// `⋀ⱼ GF fairⱼ`. Empty recovers `verify-liveness` exactly.
    #[arg(long = "fairness", value_name = "ATOM")]
    fairness: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu btor2 verify-recoverability` — `AG EF good`.
#[derive(Args, Debug)]
struct Btor2VerifyRecoverabilityArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// The `good` atom to recover to — a register comparison, e.g. `"state_q == 3"`.
    #[arg(long, value_name = "ATOM")]
    target: String,
    /// Extra abstraction predicate(s) for the cube-path escalation,
    /// `[NAME:]REG == VALUE` (repeatable, like `btor2 cegar`). Used only when the exact
    /// engine abstains (over the bit cap — 40 by default, auto-raised to 192); the escalation is automatic even
    /// with none.
    #[arg(long = "predicate", value_name = "[NAME:]REG == VALUE")]
    predicate: Vec<String>,
    /// Also emit a structured `refinement` alongside the verdict: a `vacuous` witness when the target
    /// is never reachable (the `AG EF` is degenerate), an auto `config_partition` over the design's
    /// detected reset when recovery depends on it (held-in-reset vs operational), and a best-effort
    /// "why ⊥ / what would decide it" hint. Diagnostic-only — it never changes the canonical verdict.
    #[arg(long = "refine")]
    refine: bool,
    /// Assumption discovery (refined-verdicts capability B): when the property does NOT hold, search
    /// for an environment assumption φ (a single narrow input held at a value) under which it becomes a
    /// NON-VACUOUS HOLDS → the refinement reports `holds_under`. CONDITIONAL-only: it never changes the
    /// canonical verdict (assumptions are not monotone for `AG EF`). Implies the refined output; opt-in
    /// (it costs extra decide runs).
    #[arg(long = "discover-assumptions")]
    discover_assumptions: bool,
    /// Config-partition (refined-verdicts capability A): name config INPUTS to split the verdict over,
    /// each `NAME=v1,v2,...` (repeatable). The refinement then reports a `config_partition` — "holds
    /// for configs {A}, violated for {B}" — decided exactly per config (sound per cell), every named
    /// value included: a single value (`rst_n=1`) is a scoped pin, reported as its one cell, and a
    /// violated cell is what `--discover-assumptions` searches under. Implies the refined output.
    /// Best for a NARROW / few-value config (the cross-product is capped).
    #[arg(long = "partition", value_name = "NAME=v1,v2,...")]
    partition: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu btor2 check-fsm` — the auto FSM-recoverability scan.
#[derive(Args, Debug)]
struct Btor2CheckFsmArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// Max state-register width to treat as an FSM (wider = datapath/counter, skipped).
    #[arg(long, value_name = "BITS", default_value_t = mununu_core::adapter::fsm_scan::DEFAULT_FSM_MAX_WIDTH)]
    max_width: u32,
    /// Also run the liveness inventory: legal encodings never reached, reachable encodings the
    /// register can never leave (absorbing), and whether it always returns to its reset value.
    /// Out of reset, resets pinned inactive. Report-only: the exit code is the encoding scan's.
    #[arg(long)]
    liveness: bool,
    /// An intended absorbing state (`REG=VALUE`, decimal or 0x-hex), listed as expected rather
    /// than absorbing. Repeatable; needs `--liveness`.
    #[arg(long, value_name = "REG=VALUE", requires = "liveness")]
    terminal: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
}

/// The two-player game winning objective (`btor2 game --objective`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum GameObjective {
    /// Force `good` at least ONCE — the reachability game `μX. good ∨ ⟨ctrl⟩X`.
    Reach,
    /// Force `good` INFINITELY OFTEN — the Büchi game `νZ.μY. (good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y`.
    Recurrence,
}

#[derive(Args, Debug)]
struct Btor2GameArgs {
    /// Path to the BTOR2 input file.
    #[arg(value_name = "BTOR2_FILE")]
    file: PathBuf,
    /// The target `good` atom the controller tries to force — a single register-comparison /
    /// combinational-output atom (`"state_q == 44"`, `"full_o == 1"`).
    #[arg(long, value_name = "REG op VALUE")]
    good: String,
    /// The winning objective: `reach` (default) = force `good` ONCE (`μX. good ∨ ⟨ctrl⟩X`); `recurrence`
    /// = force `good` INFINITELY OFTEN — the Büchi game `νZ.μY. (good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y`. Strategy
    /// extraction and assumption discovery apply to `reach` only today.
    #[arg(long = "objective", value_enum, default_value_t = GameObjective::Reach)]
    objective: GameObjective,
    /// A controller-owned primary input (repeatable). Every other primary input belongs to the
    /// (adversarial) environment. A name that is not a real primary input is rejected.
    #[arg(long = "controllable", value_name = "INPUT")]
    controllable: Vec<String>,
    /// When the game is UNREALIZABLE, search for an ENVIRONMENT ASSUMPTION under which the controller
    /// wins (the assume-guarantee wedge) — a narrow environment-input hold `e == v` making `A ⇒ G`
    /// realizable, reported in `holds_under` (CONDITIONAL — never flips the canonical `realizable`). The
    /// environment counterstrategy's forced inputs are the blockers, searched first.
    #[arg(long = "discover-assumptions")]
    discover_assumptions: bool,
    /// Model the CLOCK and RESET as a sound posture instead of adversarial inputs (the raw lifted BTOR2
    /// carries `clk`/`rst` as free inputs, so a two-player game otherwise lets the environment FREEZE THE
    /// CLOCK or HOLD RESET — spuriously unrealizable for a modeling, not functional, reason). Pins the
    /// detected reset inactive (+ post-reset init) and the clock to a constant, so the game is the genuine
    /// functional one. Recommended for real RTL.
    #[arg(long = "assume-clock-reset")]
    assume_clock_reset: bool,
    /// The k-fault adversary as a GAME: the fault inputs of `--fault REG[:k]` (one global budget,
    /// default 1) are added to the CONTROLLABLE inputs, so "the controller" is the attacker and
    /// the environment is the honest design's inputs. `realizable` then means an attack wins
    /// against EVERY environment behaviour (the strategy is the attack) and the CI verdict is
    /// `violated`; unrealizable means a positional environment counterstrategy blocks every
    /// attack (the counterstrategy is the defence) and the verdict is `holds`. Without
    /// `--fault` nothing changes. Mirrored on the API as `faults`.
    #[arg(long = "fault", value_name = "REG[:k]")]
    fault: Vec<String>,
    /// Cycles before the first injection may fire (default 10).
    #[arg(long = "fault-warmup", value_name = "CYCLES", default_value_t = 10)]
    fault_warmup: u32,
    #[command(flatten)]
    ci: CiArgs,
}

#[derive(Subcommand, Debug)]
enum MemoryCommand {
    /// Analyse a `verify.toml` for memory-abstraction-posture issues.
    Check(MemoryCheckArgs),
}

#[derive(Args, Debug)]
struct MemoryCheckArgs {
    /// Path to the `verify.toml` project config.
    #[arg(value_name = "VERIFY_TOML")]
    config: PathBuf,
    /// Emit the report as JSON on stdout instead of the
    /// human-readable summary.
    #[arg(long)]
    json: bool,
    /// Exit with a non-zero status if any warning is raised.
    #[arg(long)]
    strict: bool,
}

#[derive(Subcommand, Debug)]
enum LibraryCommand {
    /// List every shipped library template + a one-line summary.
    List,
    /// Emit one template's CTXDSL body to stdout (or `--output`).
    Emit(LibraryEmitArgs),
}

#[derive(Args, Debug)]
struct LibraryEmitArgs {
    /// Template name (e.g. `plic`, `watchdog`, `tracked_memory`).
    #[arg(value_name = "NAME")]
    name: String,
    /// Substitute `{instance_id}` with this value before emitting.
    /// When omitted, the placeholder is preserved verbatim — useful
    /// when feeding the output to a `[[sources]]` block with
    /// `count = N` (the verify framework substitutes per instance).
    #[arg(long, value_name = "ID")]
    instance_id: Option<String>,
    /// Output path. Defaults to stdout.
    #[arg(long, short = 'o', value_name = "PATH")]
    output: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum ContractCommand {
    /// Validate a contract set's discharge graph.
    Validate(ContractValidateArgs),
    /// Inspect a gap-marker report (diagnostics + sidecar + strict gate).
    Gaps(ContractGapsArgs),
    /// Discover a phase-1 contract from a black-box interface description.
    Discover(ContractDiscoverArgs),
    /// Emit interface + gap-report sidecars for a list of black-box modules.
    Sidecars(ContractSidecarsArgs),
    /// Query the contract corpus for matching entries.
    Query(ContractQueryArgs),
    /// HITL stage-4 — surface proposed clauses for human review.
    Review(ContractReviewArgs),
}

#[derive(Args, Debug)]
struct ContractReviewArgs {
    /// Path to the black-box interface JSON (Phase 1 discovery input).
    #[arg(value_name = "INTERFACE")]
    interface: PathBuf,
    /// Optional contract corpus root used to resolve
    /// `@mununu_interface contract://` URIs into reference proposals.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
    /// Emit the package as JSON to stdout.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ContractQueryArgs {
    /// `<domain>/<name>` identifier.
    #[arg(value_name = "DOMAIN/NAME")]
    id: String,
    /// Corpus root directory.
    #[arg(long, value_name = "DIR", default_value = "corpus")]
    corpus: PathBuf,
    /// Parameters to match, as `key=jsonvalue` pairs.
    #[arg(long = "param", value_name = "KEY=VALUE", num_args = 0..)]
    params: Vec<String>,
    /// Output as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct VerifyArgs {
    /// Path to the `verify.toml` project config.
    #[arg(value_name = "VERIFY_TOML")]
    config: PathBuf,
    /// Override the base directory used to resolve relative paths in
    /// the config. Defaults to the config file's parent directory.
    #[arg(long, value_name = "DIR")]
    base_dir: Option<PathBuf>,
    /// Emit the full `VerifyReport` as JSON on stdout instead of the
    /// human-readable summary table.
    #[arg(long)]
    json: bool,
    /// A `violated` property fails the gate (exit 2 by default); a vacuous conditional verdict
    /// (its assumptions admit no fair path) counts as `unknown` (exit 3 under `--fail-on
    /// unknown`).
    #[command(flatten)]
    ci: CiArgs,
    /// Skip property evaluation and instead emit an introspection
    /// report: per-automaton alphabet + state names, the composition's
    /// union alphabet, and every declared per-state predicate. Use
    /// this before authoring property formulas to discover what
    /// labels and predicates the realized context actually exposes.
    /// It decides nothing, so `--fail-on` has no effect with it.
    #[arg(long = "print-alphabet")]
    print_alphabet: bool,
    /// For every violated property, print the counterexample witness
    /// attached by the orchestrator (initial state → labelled steps →
    /// termination). Default off so existing transcripts stay
    /// byte-stable; opt-in for debugging.
    #[arg(long = "print-counterexample")]
    print_counterexample: bool,
    /// R4W-3 (R.4 clustered-COI) — Jaccard similarity floor for the
    /// clustered cone-of-influence comparison the BTOR2 (`sv-yosys`)
    /// route reports per source. Overrides any `cluster_similarity_floor`
    /// set in the `verify.toml`. Omitted → the recommended `0.5`.
    /// Tighter (→ `1.0`) approaches per-property COI; looser (→ `0.0`)
    /// collapses toward joint COI. Only affects `sv-yosys` sources with
    /// declared properties; a no-op for other adapters.
    #[arg(long = "cluster-coi-floor", value_name = "FLOAT")]
    cluster_coi_floor: Option<f64>,
    /// mununu#594 — the counterexample witness's step cap. Overrides any
    /// `counterexample_max_steps` set in the `verify.toml`; omitted → 20.
    /// On the safety shape `nu X. (phi && [] X)` the witness is a shortest
    /// path to a `!phi` state and rarely needs more; elsewhere it bounds
    /// the forward walk, and `(length-limit (truncated))` says it hit.
    #[arg(long = "counterexample-max-steps", value_name = "N")]
    counterexample_max_steps: Option<usize>,
}

#[derive(Args, Debug)]
struct ContractSidecarsArgs {
    /// Path to a JSON file containing a list of `BlackBoxInterface` objects.
    #[arg(value_name = "INTERFACES")]
    interfaces: PathBuf,
    /// Directory to write the sidecar files; created if missing.
    #[arg(long, value_name = "DIR")]
    out_dir: PathBuf,
    /// Emit an additional `Fairness` gap marker per module.
    #[arg(long)]
    emit_fairness_gap: bool,
    /// Optional contract corpus root used to resolve
    /// `@mununu_interface contract://` URIs on each interface. When
    /// supplied, the resolution outcomes are embedded into the gap-
    /// report sidecar so HITL UX can show vendor-declared corpus refs.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ContractValidateArgs {
    /// Path to the contract set JSON.
    #[arg(value_name = "CONTRACT_SET")]
    contract_set: PathBuf,
    /// Output the verdict as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ContractDiscoverArgs {
    /// Path to the black-box interface JSON.
    #[arg(value_name = "INTERFACE")]
    interface: PathBuf,
    /// Force these labels to Controllable.
    #[arg(long, value_name = "LABELS")]
    force_controllable: Vec<String>,
    /// Force these labels to Uncontrollable.
    #[arg(long, value_name = "LABELS")]
    force_uncontrollable: Vec<String>,
    /// Emit an additional `Fairness` gap marker.
    #[arg(long)]
    emit_fairness_gap: bool,
    /// Output as JSON.
    #[arg(long)]
    json: bool,
    /// Fail with non-zero exit if any gap.
    #[arg(long)]
    strict_contracts: bool,
    /// Write `.contract.todo.json` sidecar next to the source.
    #[arg(long, value_name = "SOURCE")]
    write_sidecar: Option<PathBuf>,
    /// Optional contract corpus root used to resolve
    /// `@mununu_interface contract://` URIs on the interface.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ContractGapsArgs {
    /// Path to the gap-marker report JSON.
    #[arg(value_name = "GAP_REPORT")]
    gap_report: PathBuf,
    /// Fail with a non-zero exit code if the report contains any gap.
    #[arg(long)]
    strict_contracts: bool,
    /// Write a `.contract.todo.json` skeleton next to the source file.
    #[arg(long, value_name = "SOURCE")]
    write_sidecar: Option<PathBuf>,
    /// Emit the report as JSON to stdout.
    #[arg(long)]
    json: bool,
}

// ============================================================================
// Codesign — Document C task C2 (slice 1)
// ============================================================================

#[derive(Subcommand, Debug)]
enum CodesignCommand {
    /// Emit the coupling CTXDSL fragment for a register-map sidecar.
    ///
    /// Reads the register-map JSON (Document C task C1 schema, see
    /// `tools/register_map_schema.json`) and emits a CTXDSL fragment
    /// containing the alphabet of rendezvous labels, a chaotic-stub
    /// peripheral automaton, and an asynchronous composition block.
    /// The user pastes the fragment into a hand-authored
    /// `context <name> { … }` block alongside their firmware
    /// automaton.
    Couple(CodesignCoupleArgs),
    /// Emit a standalone chaotic-stub CTXDSL document from a register-map
    /// sidecar.
    ///
    /// Generates the one-state-self-loops form: a single `Chaotic`
    /// state with self-loops on every rendezvous label derived from
    /// the register map. The output is a complete CTXDSL document
    /// (with its own `context { … }` wrapper) ready to reference as
    /// a `ctxdsl` source from a `verify.toml`. Sound for safety,
    /// optimistic for liveness (Doc C §C.5). See `docs/abstraction.md`.
    EmitChaoticStub(CodesignEmitChaoticStubArgs),
    /// Compose a register-map sidecar with a firmware CTXDSL and
    /// verify a property over the result.
    ///
    /// Document C task C4. Reads the register-map JSON and a
    /// firmware CTXDSL document, splices the coupling fragment into
    /// the firmware's outer context block, realises the composed
    /// document, and evaluates the named formula over the
    /// codesign-composed automaton. Counterexample classification
    /// via the C3 trace origin classifier is wired in too.
    Verify(CodesignVerifyArgs),
    /// Import a CMSIS-SVD file and emit one mununu register-map JSON
    /// per peripheral.
    ///
    /// Document C task C6. The `sv_signal` and `c_accessor` fields on
    /// each imported field start empty — CMSIS-SVD does not carry that
    /// information, and the user authors it post-import.
    ImportSvd(CodesignImportSvdArgs),
    /// Extract C function declarations + their `@mununu_*` annotations
    /// from a C source file via a `clang -ast-dump=json` shell-out.
    ///
    /// Document C task C5, slice 2.a. Emits one record per function
    /// (with attached annotations) plus a list of orphan annotations.
    /// Function bodies are NOT modelled into an automaton yet — that's
    /// slice 2.b.
    ExtractC(CodesignExtractCArgs),
    /// Emit a CMSIS-DEVICE-style C header for one peripheral from an
    /// SVD file. Phase L8 — lets `mununu codesign extract-c` consume
    /// upstream-style firmware C that uses `NRF_TWIM0->FIELD` struct
    /// member access. The output is one header file's worth of
    /// content; redirect to disk.
    EmitCmsisHeader(CodesignEmitCmsisHeaderArgs),
    /// Reconcile firmware and peripheral rendezvous-label alphabets.
    ///
    /// Reads two JSON files, each a `[ "label_1", "label_2", … ]`
    /// array. Returns the canonical shared alphabet when the two sets
    /// match exactly, or a structured mismatch report otherwise. This
    /// is the hard gate against alphabet drift between the
    /// C-extraction's firmware automaton and the SV-extraction's
    /// peripheral automaton (Doc C §C.5: silent over-approximation
    /// across a mismatched bus is unsound for safety).
    ReconcileLabels(CodesignReconcileLabelsArgs),
}

#[derive(Args, Debug)]
struct CodesignEmitCmsisHeaderArgs {
    /// Path to the CMSIS-SVD XML file. One of `--svd` or
    /// `--register-map` is required.
    #[arg(long, value_name = "SVD")]
    svd: Option<PathBuf>,
    /// Path to a register-map JSON sidecar (the format `mununu
    /// codesign couple` consumes). Use this when the SVD has
    /// vendor extensions the importer doesn't yet handle and the
    /// register map was authored / patched by hand.
    #[arg(long = "register-map", value_name = "JSON")]
    register_map: Option<PathBuf>,
    /// Peripheral name to emit. Default: all peripherals in the
    /// source (one struct + one macro per peripheral, concatenated).
    #[arg(long, value_name = "NAME")]
    peripheral: Option<String>,
    /// Vendor prefix prepended to peripheral names. E.g.
    /// `--vendor-prefix NRF_` produces `NRF_TWIM_Type` and `NRF_TWIM0`.
    #[arg(long = "vendor-prefix", value_name = "PREFIX", default_value = "")]
    vendor_prefix: String,
}

#[derive(Args, Debug)]
struct CodesignReconcileLabelsArgs {
    /// Path to the firmware-side label JSON (a `["label_1", …]` array).
    #[arg(value_name = "FIRMWARE_JSON")]
    firmware_labels: PathBuf,
    /// Path to the peripheral-side label JSON (same shape as
    /// `FIRMWARE_JSON`). Mutually exclusive with
    /// `--peripheral-register-map` — exactly one peripheral source
    /// must be supplied.
    #[arg(value_name = "PERIPHERAL_JSON", required = false)]
    peripheral_labels: Option<PathBuf>,
    /// Path to a register-map sidecar JSON. The peripheral-side
    /// alphabet is derived directly via
    /// `coupling::register_map_labels` (the same function the
    /// firmware emitter targets), so passing a register map here
    /// short-circuits any manual hand-authoring of the peripheral
    /// labels list. Mutually exclusive with `PERIPHERAL_JSON`.
    #[arg(long = "peripheral-register-map", value_name = "REGISTER_MAP_JSON")]
    peripheral_register_map: Option<PathBuf>,
    /// Output format. `human` (default): one section per outcome,
    /// human-readable. `json`: machine-parseable
    /// `{ "shared": […], "mismatch": null | { "firmware_only": […],
    /// "peripheral_only": […] } }`.
    #[arg(long, value_name = "FORMAT", default_value = "human")]
    format: String,
}

#[derive(Args, Debug)]
struct CodesignExtractCArgs {
    /// Path to the C source file (`.c` or `.h`).
    #[arg(value_name = "FILE")]
    file: PathBuf,
    /// Path to the clang binary. Defaults to `clang` (PATH).
    #[arg(long, value_name = "PATH")]
    clang: Option<PathBuf>,
    /// Include paths (repeatable) passed to clang as `-I`.
    #[arg(long = "include", value_name = "DIR")]
    include_paths: Vec<PathBuf>,
    /// Preprocessor defines (repeatable), e.g. `--define HAVE_DMA=1`.
    #[arg(long = "define", value_name = "DEF")]
    defines: Vec<String>,
    /// Extra arguments to pass to clang verbatim (advanced).
    #[arg(long = "clang-arg", value_name = "ARG")]
    extra_clang_args: Vec<String>,
    /// Treat any warning (orphan annotation, unhandled kind) as a
    /// hard error.
    #[arg(long)]
    strict: bool,
    /// Path to a register-map JSON file. When supplied (slice 2.b),
    /// each function body is walked for register accesses and the
    /// matched accessors are emitted on the function's `accesses` field.
    #[arg(long = "register-map", value_name = "JSON")]
    register_map: Option<PathBuf>,
    /// When set together with `--register-map`, synthesise a linear
    /// CTXDSL automaton from each function's register-access sequence.
    /// The automaton is emitted on the function's `automaton_ctxdsl`
    /// field and synchronises with the peripheral chaotic stub on the
    /// `coupling`-module rendezvous labels.
    #[arg(long = "synthesize-automaton")]
    synthesize_automaton: bool,
    /// Phase L7: emit a top-level Driver automaton that non-
    /// deterministically dispatches to each non-ISR entry point.
    /// Useful for driver files with multiple entry points the
    /// application calls in arbitrary order. Disabled by default.
    #[arg(long = "driver-mode")]
    driver_mode: bool,
    /// Phase L8: include the bundled vendor-neutral CMSIS-minimal
    /// stubs (`__IO`, `__NOP`, NVIC no-ops, …) in the clang
    /// invocation. Use together with `--include` pointing at an
    /// SVD-derived CMSIS header (emit via `mununu codesign
    /// emit-cmsis-header`) when the C source uses
    /// `PERIPHERAL->FIELD` struct-member access.
    #[arg(long = "cmsis-stubs")]
    cmsis_stubs: bool,
}

#[derive(Args, Debug)]
struct CodesignImportSvdArgs {
    /// Path to the CMSIS-SVD XML file.
    #[arg(value_name = "SVD")]
    svd: PathBuf,
    /// Output directory for the imported register-map JSON files.
    /// One file per peripheral: `<peripheral>.json`. Default: write
    /// JSON to stdout (no files created).
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// Import only the named peripheral (case-sensitive). Default:
    /// import every peripheral in the SVD.
    #[arg(long, value_name = "NAME")]
    peripheral: Option<String>,
    /// Treat any structural warning (derivedFrom not resolved,
    /// register array not expanded, unknown field access) as a hard
    /// error.
    #[arg(long)]
    strict: bool,
}

#[derive(Args, Debug)]
struct CodesignVerifyArgs {
    /// Path to the register-map JSON sidecar (Document C task C1).
    #[arg(value_name = "REGISTER_MAP")]
    register_map: PathBuf,
    /// Path to the firmware CTXDSL document. Must contain a single
    /// `context <name> { … }` block with at least one firmware
    /// automaton; the coupling fragment is spliced into that block.
    #[arg(value_name = "FIRMWARE_CTXDSL")]
    firmware: PathBuf,
    /// Name of the formula to evaluate (declared in the firmware
    /// document's `mu_formulas { … }` section).
    #[arg(long, value_name = "FORMULA")]
    formula: String,
    /// Composition or automaton name to evaluate the formula over.
    /// Default: the codesign-composition name emitted by the splicer
    /// (`<PERIPHERAL>System`).
    #[arg(long, value_name = "NAME")]
    automaton: Option<String>,
    /// Override the peripheral automaton name (default: uppercased
    /// peripheral name from the sidecar).
    #[arg(long, value_name = "NAME")]
    peripheral_automaton: Option<String>,
    /// Override the composition name (default: `<PERIPHERAL>System`).
    #[arg(long, value_name = "NAME")]
    composition_name: Option<String>,
    /// Emit the composed CTXDSL to this path (does not affect
    /// verification; useful for inspection / debugging).
    #[arg(long, value_name = "PATH")]
    emit_ctxdsl: Option<PathBuf>,
    /// Emit the result as JSON to stdout.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct CodesignEmitChaoticStubArgs {
    /// Path to the register-map JSON sidecar.
    #[arg(value_name = "REGISTER_MAP")]
    register_map: PathBuf,
    /// Output path. Defaults to stdout when omitted.
    #[arg(long, short = 'o', value_name = "PATH")]
    output: Option<PathBuf>,
    /// Override the peripheral automaton name (default: uppercased
    /// peripheral name from the sidecar). The context-block name is
    /// always `<AutomatonName>ChaoticStub`.
    #[arg(long, value_name = "NAME")]
    peripheral_automaton: Option<String>,
    /// Validate the register-map and exit non-zero if any issue is
    /// reported, instead of just printing warnings.
    #[arg(long)]
    strict: bool,
}

#[derive(Args, Debug)]
struct CodesignCoupleArgs {
    /// Path to the register-map JSON sidecar.
    #[arg(value_name = "REGISTER_MAP")]
    register_map: PathBuf,
    /// Override the peripheral automaton name (default: uppercased
    /// peripheral name from the sidecar).
    #[arg(long, value_name = "NAME")]
    peripheral_automaton: Option<String>,
    /// Override the composition name (default: `<PERIPHERAL>System`).
    #[arg(long, value_name = "NAME")]
    composition_name: Option<String>,
    /// Names of firmware automata to include in the composition.
    /// Pass multiple via repeated flags or a comma-separated list.
    #[arg(long = "firmware-member", value_name = "AUTOMATON")]
    firmware_members: Vec<String>,
    /// Validate the register-map and exit non-zero if any issue is
    /// reported, instead of just printing warnings.
    #[arg(long)]
    strict: bool,
}

#[derive(Subcommand, Debug)]
enum ExtractionCommand {
    /// Validate an extraction spec's line anchors against the actual source file.
    Validate(ExtractionValidateArgs),
    /// Check provenance headers in CTXDSL files.
    Check(ExtractionCheckArgs),
}

#[derive(Args, Debug)]
struct ExtractionValidateArgs {
    /// Path to extraction spec JSON (.json or .espec.json).
    #[arg(value_name = "SPEC")]
    spec: PathBuf,
    /// Path to the source file referenced by the spec.
    #[arg(value_name = "SOURCE")]
    source: PathBuf,
    /// Output as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,
    /// Number of lines to search around expected position for drifted anchors.
    #[arg(long, default_value = "5")]
    drift_window: usize,
}

#[derive(Args, Debug)]
struct ExtractionCheckArgs {
    /// CTXDSL files to check for provenance headers.
    #[arg(value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,
    /// Require @generated-from header (fail if missing).
    #[arg(long)]
    require_generated: bool,
    /// Require @model-source header (fail if missing).
    #[arg(long)]
    require_model_source: bool,
}

#[derive(Subcommand, Debug)]
enum SvCommand {
    /// Preprocess SystemVerilog through sv2v into Verilog-2005.
    ///
    /// Standalone wrapper over the sv2v binary (zachjs/sv2v). Elaborates
    /// SV-2017 constructs (generates, interfaces, structs, always_ff,
    /// parameterized modules, …) into a Verilog-2005 subset, preserving
    /// module hierarchy and signal names. Output goes to --output or
    /// <stem>.elab.v next to the input.
    ///
    /// Used by the KMTS pipeline (R.0a) as the frontend normaliser
    /// before Yosys-no-flatten. Also exposed standalone for users who
    /// want to inspect or further-process the sv2v output. Same
    /// sv2v invocation as the Yosys path's `--preprocessor sv2v`.
    Preprocess(SvPreprocessArgs),
    /// Emit one BTOR2 per submodule (R.0b — KMTS pipeline frontend).
    ///
    /// Runs Yosys with `hierarchy -check` (no `flatten`) and emits one
    /// BTOR2 per submodule reachable from the top. The per-submodule
    /// BTOR2 files feed the R.2 KMTS lifter; the top-module netlist
    /// drives composition (see docs/design/native-sv-abstraction.md
    /// §3 + §4).
    ///
    /// Output: writes `<output-dir>/<module>.btor2` per submodule.
    /// Defaults to writing into the same directory as the input file
    /// when --output-dir is omitted. Also prints the per-submodule
    /// state-count / property-count summary to stdout.
    ///
    /// SCOPE: this runs one full Yosys elaboration PER submodule (re-reading
    /// the whole source set each time) and is built for SMALL multi-module
    /// fixtures. A large real-RTL design can exhaust time or memory; each
    /// per-module Yosys call is capped at 60s and fails with an actionable
    /// error rather than hanging. For a large design, use the whole-design
    /// path (`sv verify-auto`, or a single `write_btor`).
    EmitBtor2PerModule(SvEmitBtor2PerModuleArgs),
    /// Validate a `.mununu.json` sidecar (sidecar-audit C0.2).
    ///
    /// Runs the same load-time lint the CLI / API / verify paths apply
    /// automatically (C0.1): hard-fails on a removed `$schema`, warns on
    /// unknown fields at the root / signals[] / inputs[] / properties[]
    /// levels (a likely typo that would otherwise deserialize to a serde
    /// default), and tolerates `$`/`_`-prefixed comment keys. This is the
    /// standalone way to check a sidecar without running a full extraction.
    ///
    /// surface: CLI-only — a developer convenience that surfaces the
    /// shared `lint_annotation_json` check; that check already runs on
    /// every sidecar load across CLI + API + verify, so the *capability*
    /// is on all surfaces. The API/UI validation peer lands with the C2.2
    /// sidecar-editor panel where it has a consumer.
    Validate(SvValidateArgs),
    /// Discover a design's state cells and emit a skeleton sidecar
    /// (sidecar-audit C1.1 / finding E1).
    ///
    /// Drives sv2v → Yosys-flatten → BTOR2 and prints a `.mununu.json`
    /// skeleton pre-populated with the design's real post-`flatten`
    /// dotted-instance state-cell names (`u_chan0.prediv_q`) — the names
    /// the bit-blaster and a param-concretization sidecar key on. Removes
    /// the manual "run yosys by hand, read the netlist, transcribe the
    /// names" step the R46-6 GAP-2 fixtures required.
    ///
    /// Unlike a verify/extract run, discovery does NOT bit-blast, so it
    /// succeeds on cap-busting designs — exactly when you need the names to
    /// write a concretization sidecar. Multi-bit cells are emitted as
    /// `ignored` placeholders with a width note (edit to concretize:
    /// `bounded_counter` / `enum` / keep `ignored`); 1-bit cells are
    /// omitted (the bit-blaster handles them natively). The skeleton JSON
    /// goes to stdout (or --output); a human summary goes to stderr.
    ///
    /// surface: CLI-only — a developer authoring aid for the file-based
    /// `.mununu.json` workflow, alongside `sv preprocess` /
    /// `sv emit-btor2-per-module`. The API/UI authoring peer lands with the
    /// C2.2 sidecar-editor panel.
    Discover(SvDiscoverArgs),
    /// SV-direct CEGAR in one command (cegar-extraction Stage 2).
    ///
    /// Lifts SystemVerilog to a single flattened BTOR2 (sv2v + Yosys) and
    /// runs the same predicate-abstraction refinement loop as
    /// `mununu btor2 cegar`, printing the per-iteration trace + 3-valued
    /// verdict. Removes the manual "run `sv emit-btor2-per-module` first,
    /// then `btor2 cegar`" two-step. Surface peer of the API
    /// `POST /api/v1/sv/cegar` and the extraction-tab SV → CEGAR flow.
    Cegar(SvCegarArgs),
    /// Extract SystemVerilog Assertions and translate them to mu-calculus
    /// (Track-H SVA front-end, XL.6a).
    ///
    /// Runs the open-source `slang` parser (`slang --ast-json`) over the SV
    /// source, finds every `assert` / `assume` / `cover property`, and
    /// translates the supported Tier-1/Tier-2 fragment to mu-calculus formulas
    /// the verifier can check — emitting each cover's `AG EF` recoverability
    /// companion and the `$past` shadow registers the formulas need. Anything
    /// outside the fragment is reported unsupported, never silently dropped.
    /// No model verification yet (that is `sv verify-auto`). Surface peer of
    /// `POST /api/v1/sv/extract-sva` + the extraction-tab SVA panel.
    ExtractSva(SvExtractSvaArgs),
    /// Automated SVA verification — no sidecar (Track-H, XL.6b).
    ///
    /// The headline no-sidecar verify: extract the design's SVA (slang) → lift
    /// SV → BTOR2 (sv2v + Yosys) → synthesize `$past` shadow flops → for each
    /// translated property, auto-seed cube predicates from the formula's
    /// state-cell atoms and run the predicate-abstraction refinement loop,
    /// printing a per-property verdict. Properties whose atoms reference
    /// non-state signals (combinational/IO) are reported skipped (the cube path
    /// can't bind them) — never given a misleading verdict. Surface peer of
    /// `POST /api/v1/sv/verify-auto` + the extraction-tab verify-auto panel.
    VerifyAuto(Box<SvVerifyAutoArgs>),
    /// Lift SV (sv2v + Yosys) and decide `bad`-reachability of the `bad` lines yosys lowers its
    /// immediate assertions to, with the multi-engine safety portfolio — one call, no
    /// `emit-btor2` step. Not the SVA property set: that is `sv verify-auto`. (0.5: renamed from
    /// `sv verify`.) Surface peer of `POST /api/v1/sv/verify-bad`.
    #[command(name = "verify-bad")]
    Verify(SvVerifyArgs),
    /// Lift SV and decide liveness — the SV-direct peer of `btor2 verify-liveness`:
    /// `--request A --grant G` (`AG(A → AF G)`), with repeatable `--fairness F` (under
    /// `⋀ GF F`), or repeatable `--response "A => G"` (the conjunction; 0.5 merges
    /// `sv verify-liveness-all` here). Atoms are single register comparisons. Surface peer of
    /// `POST /api/v1/sv/verify-liveness`.
    VerifyLiveness(SvVerifyLivenessArgs),
    /// Lift SV and decide recoverability `AG EF good` — the branching property SVA
    /// cannot state, checked directly against raw SV in one call. `--target` is a
    /// single register-comparison atom. Surface peer of
    /// `POST /api/v1/sv/verify-recoverability`.
    VerifyRecoverability(SvVerifyRecoverabilityArgs),
    /// Lift SV and auto-scan every FSM-like state register for a reachable illegal
    /// encoding — no input. The SV-direct one-call peer of `btor2 check-fsm`: derives
    /// each register's legal encodings from the design and reports any register that can
    /// reach a value outside its enum (an unambiguous bug). Surface peer of
    /// `POST /api/v1/sv/check-fsm`.
    CheckFsm(SvCheckFsmArgs),
    /// Lift SV and report — at CI time (~lift cost, no model checking) — every
    /// register whose partial-write lift the verifier cannot keep faithfully
    /// (monono#partsel): a plain-vector `q[hi:lo] <= d` whose unwritten bits the
    /// front-end models as free inputs. These are exactly the registers a state
    /// predicate would be *refused* on (skipped, never mis-decided) by the formal
    /// gate — surfaced in ~0.1 s before the minutes-long verify. Read-only, changes
    /// no verdict. Surface peer of `POST /api/v1/sv/lint`.
    ///
    /// Exit codes (per the `--fail-on` gate default): 0 = no findings; 2 = at
    /// least one finding (a gate-friendly default so `sv lint` fits directly into
    /// CI). A LIFT FAILURE (front-end error, missing file, etc.) exits 1 —
    /// distinct from "found something". A batch scanner that wants to distinguish
    /// "found a bad register" from "could not lift" should pass `--fail-on none`
    /// and inspect the JSON/text output for findings (mununu#475 item 5).
    Lint(SvLintArgs),
    /// Structural clock-domain-crossing lint: lift SV with its clock domains merged and classify
    /// every flop that samples another domain's flop — a 2-FF (or deeper) synchroniser
    /// (`holds`), logic before the first stage or a capture read by logic (`violated`), a
    /// multi-bit bus through parallel synchronisers (its gray step decided by a reach query on
    /// the source register), a qualified-data capture, a reconvergence point or a dual-clock
    /// memory (`unknown`, with the functional condition named). Pulse width across the crossing
    /// is not structural: `scripts/cdc_formal_sby.sh` checks it functionally. Surface peer of
    /// `POST /api/v1/sv/cdc-lint`. Exit codes follow `--fail-on` (default: 2 on a violated row).
    CdcLint(SvCdcLintArgs),
    /// Apply a NAMED structural mutation to the design and re-verify, checking that
    /// the property verdicts FLIP (property adequacy). A flip confirms a property
    /// constrains the mutated behaviour; a property that does NOT flip is the
    /// finding — the spec is vacuous w.r.t. that fault. `--list` enumerates the
    /// available targets. This is a statement about the PROPERTIES, never a bug
    /// report about the design. Surface peer of `POST /api/v1/sv/mutate`.
    Mutate(SvMutateArgs),
}

#[derive(Args, Debug)]
struct SvValidateArgs {
    /// Path to the `.mununu.json` sidecar to validate.
    #[arg(value_name = "SIDECAR")]
    sidecar: PathBuf,
    /// Treat warnings (unknown fields, unrecognized `$schema`) as errors —
    /// exit non-zero if any are found. Default: warnings print but exit 0.
    #[arg(long)]
    strict: bool,
}

#[derive(Args, Debug)]
struct SvDiscoverArgs {
    /// SystemVerilog source file(s). The FIRST is the primary/top source;
    /// the rest are additional sources / `\`include` targets (e.g. a
    /// package or a `prim_assert.sv` stub), staged so includes resolve —
    /// the same convention as a `verify.toml` `files` list.
    #[arg(value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,
    /// Top module name. Recommended for multi-module designs so Yosys
    /// flattens from the right root.
    #[arg(long)]
    top: Option<String>,
    /// RTL front end: `auto`, `verilog`, `slang`, or `sv2v` (sv2v, then read_verilog).
    #[arg(long = "frontend", value_enum, default_value_t = SvFrontendArg::Auto)]
    frontend: SvFrontendArg,
    /// Write the skeleton sidecar here instead of stdout.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct SvPreprocessArgs {
    /// Path(s) to the SystemVerilog source file(s) (.sv). At least one
    /// required. Multiple files are passed to sv2v in one invocation
    /// (sv2v resolves cross-file packages, interfaces, and parameter
    /// references in a single pass).
    #[arg(value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,
    /// Include directory for `\`include` resolution. Repeatable;
    /// forwarded to sv2v as `-I <dir>`.
    #[arg(short = 'I', long = "include-dir", value_name = "DIR")]
    include_dirs: Vec<PathBuf>,
    /// Output path for the elaborated Verilog-2005. Defaults to
    /// <first-stem>.elab.v next to the first input file.
    #[arg(long = "output", value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct SvEmitBtor2PerModuleArgs {
    /// Primary SystemVerilog source file (.sv) containing the top
    /// module or the multi-module design.
    #[arg(value_name = "FILE")]
    file: PathBuf,
    /// Additional SV source files providing submodules or packages.
    /// Repeatable; each is read alongside the primary input.
    #[arg(long = "source", value_name = "FILE")]
    sources: Vec<PathBuf>,
    /// Explicit top-module name. When omitted, Yosys's
    /// `hierarchy -auto-top` picks the root.
    #[arg(long = "top", value_name = "NAME")]
    top: Option<String>,
    /// Output directory for the per-submodule BTOR2 files. Defaults
    /// to the directory of the primary input.
    #[arg(long = "output-dir", value_name = "DIR")]
    output_dir: Option<PathBuf>,
    /// RTL front end: `auto`, `verilog`, `slang`, or `sv2v` (sv2v, then read_verilog).
    #[arg(long = "frontend", value_enum, default_value_t = SvFrontendArg::Auto)]
    frontend: SvFrontendArg,
    /// Use Yosys's `setundef -anyseq` instead of the default
    /// `setundef -zero`. Preserves CWE-1245-class semantics at the
    /// cost of introducing `$anyseq` state cells.
    #[arg(long = "setundef-anyseq")]
    setundef_anyseq: bool,
    /// R-Y1 (§Phase 8) — Use Yosys's `setundef -anyconst` instead of
    /// the default `setundef -zero`. Adds one nondeterministic
    /// **constant input** per undef bit (NOT per-cycle state cells).
    /// Preserves CWE-1245-class bug-bearing semantics at zero extra
    /// state-cell cost — the intermediate between `-zero` (masks
    /// bugs) and `-anyseq` (state-space explosion). When both
    /// `--setundef-anyseq` and `--setundef-anyconst` are passed,
    /// `--setundef-anyseq` wins (strictly more permissive).
    #[arg(long = "setundef-anyconst")]
    setundef_anyconst: bool,
}

/// cegar-extraction Stage 2 — `mununu sv cegar`. SV-direct CEGAR in one
/// command: lift SV → single flattened BTOR2 (sv2v + Yosys) → predicate-
/// abstraction refinement loop. The CEGAR flags mirror `btor2 cegar`
/// exactly; the source half mirrors `sv emit-btor2-per-module`.
#[derive(Args, Debug)]
struct SvCegarArgs {
    /// Primary SystemVerilog source file (.sv / .v).
    #[arg(value_name = "SV_FILE")]
    file: PathBuf,
    /// Additional SV source files (packages, sub-modules, `include`
    /// targets), staged alongside the primary input. Repeatable.
    #[arg(long = "source", value_name = "FILE")]
    sources: Vec<PathBuf>,
    /// Top module name. Recommended for multi-module designs so Yosys
    /// flattens from the right root; omitted lets Yosys auto-detect.
    #[arg(long = "top", value_name = "NAME")]
    top: Option<String>,
    /// RTL front end: `auto`, `verilog`, `slang`, or `sv2v` (sv2v, then read_verilog).
    #[arg(long = "frontend", value_enum, default_value_t = SvFrontendArg::Auto)]
    frontend: SvFrontendArg,
    /// Use Yosys's `setundef -anyseq` instead of the default
    /// `setundef -zero` (per-cycle havoc on undefined nets).
    #[arg(long = "setundef-anyseq")]
    setundef_anyseq: bool,
    /// Use Yosys's `setundef -anyconst` (one nondeterministic constant
    /// input per undef bit — the Caliptra CWE-1245 power-up policy).
    /// `--setundef-anyseq` wins when both are set.
    #[arg(long = "setundef-anyconst")]
    setundef_anyconst: bool,

    // --- CEGAR flags (identical to `btor2 cegar`) ---
    /// μ-calculus formula evaluated over the lifted KMTS.
    #[arg(long, value_name = "FORMULA")]
    formula: String,
    /// Initial predicate, repeatable. Format `[NAME:]REG == VALUE`.
    /// At least one is required to bootstrap the `2^|P|` cube space.
    #[arg(long = "predicate", value_name = "[NAME:]REG == VALUE")]
    predicates: Vec<String>,
    /// Predicate-discovery source on `KleeneBot` refinement.
    #[arg(long, value_enum, default_value_t = PredicateSourceArg::Wp)]
    predicate_source: PredicateSourceArg,
    /// Path to the cvc5 binary (Craig interpolation). Overrides
    /// `MUNUNU_CVC5_PATH`.
    #[arg(long, value_name = "PATH")]
    cvc5_path: Option<PathBuf>,
    /// Max CEGAR iterations before bailing with the current verdict.
    #[arg(long, default_value_t = 16)]
    max_iterations: usize,
    /// Must-edge inference policy.
    #[arg(long, value_enum, default_value_t = MustEdgeInferenceArg::Off)]
    must_edge_inference: MustEdgeInferenceArg,
    /// May-edge inference policy.
    #[arg(long, value_enum, default_value_t = MayEdgeInferenceArg::SmtAllPairs)]
    may_edge_inference: MayEdgeInferenceArg,
    /// R-S8 symbolic-init values, repeatable. Format
    /// `REG=v1,v2,...` — the register's admissible power-up set.
    #[arg(long = "init-values", value_name = "REG=v1,v2,...")]
    init_values: Vec<String>,
    /// Print a JSON summary instead of the human-readable report.
    #[arg(long)]
    json: bool,
    /// R.6.6 controllability split — controller-driven input symbol,
    /// repeatable.
    #[arg(long = "controllable", value_name = "INPUT")]
    controllable_inputs: Vec<String>,
    /// Sidecar `.mununu.json` path (abstractions / simulate_reset /
    /// vcd_traces). Overrides any `--init-values` synthetic sidecar.
    #[arg(long = "sidecar", value_name = "PATH")]
    sidecar: Option<PathBuf>,
    /// CTXDSL Phase 2 — write the final refined model + formula as
    /// CTXDSL to this path (stderr confirmation; stdout stays clean).
    #[arg(long = "emit-ctxdsl", value_name = "PATH")]
    emit_ctxdsl: Option<PathBuf>,
    /// R-F5.4.2b (2026-07-03) — predicate-cube engine: `explicit` (default,
    /// SMT edges + CEGAR refinement) or `symbolic` (R-F5 BDD relation,
    /// single-shot, no per-cube-pair SMT). See `mununu btor2 cegar --help`.
    #[arg(long, value_enum, default_value_t = EngineArg::Explicit)]
    engine: EngineArg,
}

#[derive(Args, Debug)]
struct SvExtractSvaArgs {
    /// Primary SystemVerilog source file (.sv / .v).
    #[arg(value_name = "SV_FILE")]
    file: PathBuf,
    /// Additional SV sources (packages, `include` targets — e.g. the standard
    /// OpenTitan `prim_assert` macros; the dummy-macro variant silently drops
    /// all SVA). Staged alongside the primary input + on the include path.
    /// Repeatable.
    #[arg(long = "source", value_name = "FILE")]
    sources: Vec<PathBuf>,
    /// Print a JSON report instead of the human-readable summary.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct SvVerifyAutoArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    /// Max CEGAR iterations per property.
    #[arg(long, default_value_t = 16)]
    max_iterations: usize,
    /// Must-edge inference policy per property. Default `auto` (mununu#663): a
    /// Liveness/Mixed-class formula (`AG EF`, `AG AF` — a μ under a ν) runs its cube
    /// member with `smt-per-target-standard`, the rest may-only. `off` = may-only everywhere;
    /// `smt-hyper-must` gives sound νμ verdicts on every property.
    #[arg(long, value_enum, default_value_t = MustEdgeInferenceArg::Auto)]
    must_edge_inference: MustEdgeInferenceArg,
    /// Disable reset-gating: keep `disable iff (reset)` guards in the formulas
    /// and leave the reset input free (by default the guard is dropped and the
    /// reset is pinned inactive so the running design is verified).
    #[arg(long = "no-gate-reset")]
    no_gate_reset: bool,
    /// Disable auto-injection of behavioral stubs for cut flop primitives
    /// (e.g. OpenTitan's `prim_sparse_fsm_flop`). By default a cut flop is
    /// stubbed so its register survives the lift; pass this to leave it cut.
    #[arg(long = "no-auto-stub-flops")]
    no_auto_stub_flops: bool,
    /// Disable the ⊥ escalations. By default, a *safety* property the cube abstraction
    /// leaves ⊥ (and that is a reducible AG-invariant) is retried with the multi-engine
    /// reachability portfolio (exact ⊕ native ⊕ spacer ⊕ btormc ⊕ Pono), and a *box-AF
    /// liveness* property left ⊥ is retried via the liveness-to-safety reduction; pass
    /// this to report the cube's ⊥ verdict unchanged for both.
    #[arg(long = "no-rescue")]
    no_rescue: bool,
    /// mununu#629 — disable antecedent constant propagation on the exact-symbolic
    /// engine. By default a same-cycle implication `sig == K |-> C` has `K`
    /// propagated through `C`'s cone (a read mux folds to the selected arm), so a
    /// property about one field of a wide packed record decides instead of
    /// skipping on the bit cap. Pass this to leave the formula as lifted —
    /// differential-oracle / debug use. Mirrored on the API as
    /// `no_antecedent_propagate: true` and by `MUNUNU_NO_ANTECEDENT_PROPAGATE=1`.
    #[arg(long = "no-antecedent-propagate")]
    no_antecedent_propagate: bool,
    /// mununu#656 — disable inputs-in-the-state on the exact-symbolic engine. By
    /// default a property that reads a primary input, or a signal combinationally
    /// driven by one (`gnt_o = req_i & sel & ready_i`), is decided over the registers
    /// with that input sampled in the same cycle, instead of being skipped. Pass
    /// this to restore the refusal — differential-oracle / debug use. Mirrored on
    /// the API as `no_input_state: true` and by `MUNUNU_NO_INPUT_STATE=1`.
    #[arg(long = "no-input-state")]
    no_input_state: bool,
    /// How SVA `assume` statements are treated. `precondition` (default): each
    /// supported assumption (`assume property (B)`, `A |=> B` over booleans)
    /// restricts the traces the guarantees are checked on; the report says
    /// which were applied, which were not and whether they are consistent.
    /// `check`: each assumption is checked like an assertion and constrains
    /// nothing. `ignore`: dropped. Mirrored on the API as `assumes`.
    #[arg(long = "assumes", value_enum, default_value_t = AssumesArg::Precondition)]
    assumes: AssumesArg,
    /// H.J.b — config concretization: pin a wide config input to a constant so
    /// comparisons against it become decidable (e.g. a timer threshold).
    /// Repeatable; format `SIGNAL=VALUE` (e.g. `--pin
    /// cfg_detect_timer_i=7`). Verdicts are then SCOPED to these values (shown as
    /// a `config-concretization` note). Only actual inputs are pinned.
    #[arg(long = "pin", value_name = "SIGNAL=VALUE")]
    pin: Vec<String>,
    /// H.H — counter upper bound: seed a `SIGNAL <= VALUE` cube-partition to refine
    /// a counter-monotonicity property (`cnt_q >= $past(cnt_q)`) whose ⊥ is the
    /// abstract 32-bit wraparound. Repeatable; format `SIGNAL<=VALUE` (e.g.
    /// `--counter-bound cnt_q<=7`; the `SIGNAL=VALUE` spelling is also accepted).
    /// Sound (a partition, not an assumption); needs `--must-edge-inference` on.
    /// Bounds are also auto-derived from `--pin`; a manual bound overrides
    /// the inferred one. Shown as a `counter-bound` note.
    #[arg(long = "counter-bound", value_name = "SIGNAL<=VALUE")]
    counter_bound: Vec<String>,
    /// Fault-injection posture: a register the adversary may overwrite (`REG` or `REG:k`;
    /// `k` is the ONE global budget of injections, default 1 — a single-event upset;
    /// `REG:unbounded` lifts it). Repeatable; `gen[1].q` is read as `gen$1.q`. The
    /// corruption is a free value at a free cycle after `--fault-warmup`, gated
    /// structurally (no `constraint`), so every engine sees the same total model. A safety
    /// HOLDS transfers ("the countermeasure responds for every corruption at these sites
    /// within the budget"); a VIOLATED is a real escape under the declared sites and budget;
    /// an alternating (νμ) property over a corrupted register is skipped (the fault player
    /// could restore any state). Mirrored on the API as `faults`.
    #[arg(long = "fault", value_name = "REG[:k]")]
    fault: Vec<String>,
    /// Fault-injection posture from the design's own countermeasures: `sec-cm[:k]` selects,
    /// for every `$rose(X.err_o) |-> ##[0:n] alert` chain, the state cells `X` compares (the
    /// duplicated counters, the sparse-FSM state) as the sites, under one budget `k`
    /// (default 1). `sec-cm:0` selects without injecting and runs the antecedent-vacuity
    /// check only — the "before" instrument. Alternative to `--fault`. Mirrored on the API
    /// as `fault_model`.
    #[arg(long = "fault-model", value_name = "MODEL[:k]")]
    fault_model: Option<String>,
    /// Cycles before the first injection may fire (default 10, OpenTitan's own `[*10]`
    /// assumption). Mirrored on the API as `fault_warmup`.
    #[arg(long = "fault-warmup", value_name = "CYCLES", default_value_t = 10)]
    fault_warmup: u32,
    /// Abstraction-predicate hint: a predicate expression (`reg == value`,
    /// `reg == reg`, `reg >= K`) seeded as a cube dimension for EVERY property,
    /// even when it does not appear in the property formula. The command-line peer
    /// of the in-source `// @mununu_predicate <expr>` annotation (both are merged).
    /// Sound by monotonicity of predicate abstraction — a hint only refines the
    /// cube (a ⊥ can become definite; a definite verdict never flips). Repeatable
    /// (e.g. `--predicate "c_state == 0" --predicate "wptr == rptr"`).
    #[arg(long = "predicate", value_name = "EXPR")]
    predicate: Vec<String>,
    /// A property template to check besides the design's assertions (`mununu templates --domain
    /// rtl` lists them): `--template rtl_response --template-arg 'TRIGGER=trip_o == 1'
    /// --template-arg 'RESPONSE=state_q == SCRAM'`. Repeatable; each becomes a row labelled
    /// `tmpl.<instance>`.
    #[arg(long = "template", value_name = "ID")]
    template: Vec<String>,
    /// A template argument, `KEY=VALUE`; with more than one `--template`, prefix the key with the
    /// template's ID (`rtl_never.BAD=…`). Repeatable.
    #[arg(
        long = "template-arg",
        value_name = "[ID.]KEY=VALUE",
        requires = "template"
    )]
    template_args: Vec<String>,
    /// A project template pack (`TemplateCatalog` JSON) layered over the built-in catalog and the
    /// deployment's packs. Repeatable.
    #[arg(long = "templates-file", value_name = "FILE")]
    templates_files: Vec<PathBuf>,
    /// Write a Verilator replay testbench for every counterexample to DIR/<property>/
    /// (`tb.cpp` + `Makefile`; `make -C DIR/<property>` builds and runs it). It resets the design,
    /// drives the trace's inputs, compares every register the trace names after each clock edge,
    /// and runs the design's assertions under `--assert`.
    #[arg(long = "emit-testbench", value_name = "DIR")]
    emit_testbench: Option<PathBuf>,
    /// The clock port the replay testbench toggles (default: the first of `clk`, `clk_i`,
    /// `clock`, `i_clk`, `aclk`, `clk_sys` the primary source declares).
    #[arg(long = "tb-clock", value_name = "PORT", requires = "emit_testbench")]
    tb_clock: Option<String>,
    /// Print a JSON report instead of the human-readable summary.
    #[arg(long)]
    json: bool,
    /// Verify engine. **Default `portfolio-sequential`** (③a, 2026-07-23): run
    /// `exact-symbolic` → `symbolic` → `explicit` in precision order, merging and
    /// early-exiting the moment every property is decided. Exact-first because the
    /// exact engine decides a control property whose cone-of-influence fits the bit
    /// cap directly (and *cleanly skips* an over-cap cone), where the bare
    /// predicate-cube `explicit` engine can ⊥ or grind on a wide combinational cone;
    /// the cube legs still catch what exact skips (input-antecedent, over-cap). Pin a
    /// single engine with `--engine explicit` (predicate-cube + CEGAR), `symbolic`
    /// (R-F5 BDD relation, no per-cube-pair SMT), or `exact-symbolic` (EXACT over the
    /// reset-gated full bit-blast — a **definite** 2-valued verdict, never ⊥; bounded
    /// by BDD size, over-cap ⇒ `Skipped`). NB: a pinned bare `explicit` on a wide
    /// combinational cone can hang unless `MUNUNU_CUBE_SMT_RLIMIT` is set (③b).
    #[arg(long, value_enum, default_value_t = EngineArg::PortfolioSequential)]
    engine: EngineArg,
    #[command(flatten)]
    ci: CiArgs,
    #[command(flatten)]
    expect: ExpectArgs,
}

/// Shared SV → BTOR2 lift inputs for the SV-direct verbs (`sv verify-bad` /
/// `verify-liveness` / `verify-recoverability`).
#[derive(Args, Debug)]
struct SvLiftArgs {
    /// Primary SystemVerilog source file (.sv / .v). Omit when using `--design-dir`.
    #[arg(value_name = "SV_FILE", required_unless_present = "design_dir")]
    file: Option<PathBuf>,
    /// E6 — auto-assemble a multi-file design from a directory: discover every
    /// `.v`/`.sv` under DIR (skipping `mutations`/`buggy`/`tb` subtrees), work out
    /// the compilation units, the include-search dirs, and the top module
    /// (declared-minus-instantiated), so a multi-file design lifts WITHOUT hand-
    /// assembling `--source`/`--include-dir`/`--top`. An explicit `--top` overrides
    /// the detected one; `--include-dir` adds to the detected dirs. Mutually
    /// exclusive with the positional `SV_FILE`.
    ///
    /// surface: CLI-only — E6's on-disk directory scan + auto-assembly is a
    /// filesystem convenience; the API/UI accept sources by name (the caller
    /// stages them), so a directory scan has no analog there. The assembly core
    /// (`yosys::source_manifest::assemble_sv_design`) is content-based and could
    /// back a future API auto-top enhancement.
    #[arg(long = "design-dir", value_name = "DIR", conflicts_with = "file")]
    design_dir: Option<PathBuf>,
    /// mununu#475 item 2 — additional path-component names to exclude from
    /// `--design-dir` scans, on top of the hardcoded skip set
    /// (`mutations` / `buggy` / `buggy_artifacts` / `tb` / `testbench` /
    /// `sim` / `figures`). Match is case-insensitive against a single path
    /// component (`--exclude faulty` skips every `.../faulty/...` subtree
    /// but leaves `.../faulty_variant/...` alone). Repeatable. No-op without
    /// `--design-dir`. Not a full glob syntax — a richer form is a future
    /// extension when a case demands it.
    ///
    /// surface: CLI-only — like `--design-dir` and `--include-dir`, an
    /// on-disk directory-scan filter has no analog on the API/UI (their
    /// flat name-staging already lets the caller choose which sources to
    /// upload).
    #[arg(long = "exclude", value_name = "NAME")]
    exclude: Vec<String>,
    /// mununu#475 item 3 — additional module search directories for
    /// single-file lifts: recursively scans DIR for `.v` / `.sv` sources
    /// and adds them alongside the primary input, so a single-file input
    /// that instantiates a submodule from a sibling directory
    /// (`mem_sched.sv` → `slot_arbiter` from a peer dir, 53/109 unliftable
    /// files in monono's tree) resolves without hand-listing every
    /// `--source`. The same `--exclude` list filters the scan. Deduplicates
    /// against the primary file and any explicit `--source` entries by
    /// canonical path (a search-path that contains the primary file will
    /// not double-add it). Repeatable. Also composable with `--design-dir`
    /// — a primary design + sibling libraries.
    ///
    /// surface: CLI-only — like `--design-dir` / `--include-dir` /
    /// `--exclude`, an on-disk directory scan has no analog on the
    /// API/UI (their flat name-staging already lets the caller stage
    /// whichever sources they want).
    #[arg(long = "search-path", value_name = "DIR")]
    search_paths: Vec<PathBuf>,
    /// Additional SV sources (packages, `include` targets), staged alongside the
    /// primary input. Repeatable.
    #[arg(long = "source", value_name = "FILE")]
    sources: Vec<PathBuf>,
    /// Top module for the SV → BTOR2 Yosys lift (auto-detect when omitted).
    #[arg(long = "top", value_name = "NAME")]
    top: Option<String>,
    /// Lift a multi-clock design onto the model's one implicit clock instead of refusing it.
    /// An UNDER-approximation: the domains only ever tick together, so a crossing defect cannot
    /// occur and a HOLDS does not transfer (reported as the `clock-domains-merged` soundness
    /// caveat).
    #[arg(long = "merge-clock-domains")]
    merge_clock_domains: bool,
    /// Extra include-search directory (`-I<dir>`), repeatable. Resolves
    /// `` `include "frag.vh" `` against the original source tree so include
    /// fragments need NOT be passed as standalone `--source` compilation units
    /// (a mid-module fragment parsed in isolation fails). The per-design
    /// source-manifest multi-file lift uses this.
    ///
    /// surface: CLI-only — the API/UI pass source content by name, so an
    /// on-disk include-search directory has no analog there; their flat
    /// name-staging of additional sources already resolves cross-file includes.
    #[arg(long = "include-dir", value_name = "DIR")]
    include_dirs: Vec<PathBuf>,
    /// RTL front-end for the lift. `slang` forces the yosys-slang plugin
    /// (`read_slang`), which lifts modern-SV constructs `read_verilog` and sv2v
    /// reject (`while` loops, `module M import pkg::*;`). Requires the
    /// yosys-slang plugin (present in the mununu-sva image). `auto` (default)
    /// keeps the env-driven behaviour; `verilog` forces read_verilog; `sv2v` runs sv2v first
    /// (modern SV the plain `read_verilog` refuses: `import pkg::*;`, structs, interfaces).
    #[arg(long = "frontend", value_enum, default_value_t = SvFrontendArg::Auto)]
    frontend: SvFrontendArg,
    /// Override a module parameter, `NAME=VALUE` or `MODULE.NAME=VALUE` (repeatable). Shrinks a
    /// parameterised design (a FIFO depth, a counter width) to a decidable size. A malformed
    /// entry is an error. API: `lift.params`.
    #[arg(long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// Replace a source for the LIFT only, `NAME=PATH` (repeatable): an assertion-macro shim or a
    /// synthesis variant. `sv verify-auto` still extracts its SVA from the original sources. A
    /// name that matches no source is an error. API: `lift.lift_overrides`.
    #[arg(long = "lift-override", value_name = "NAME=PATH")]
    lift_override: Vec<String>,
    /// Control-slice cut point (repeatable): the named net becomes a free input, an
    /// over-approximation that drops its fan-in cone. API: `lift.cutpoints`.
    #[arg(long = "cutpoint", value_name = "SIGNAL")]
    cutpoint: Vec<String>,
}

/// Arguments for `mununu sv verify-bad` — SV-direct safety portfolio.
#[derive(Args, Debug)]
struct SvVerifyArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu sv verify-liveness` — SV-direct response liveness.
#[derive(Args, Debug)]
struct SvVerifyLivenessArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    /// The request atom — a register comparison, e.g. `"st == 1"`.
    #[arg(
        long,
        value_name = "ATOM",
        requires = "grant",
        conflicts_with = "responses"
    )]
    request: Option<String>,
    /// The grant atom that must eventually follow on every path, e.g. `"st == 2"`.
    #[arg(long, value_name = "ATOM", requires = "request")]
    grant: Option<String>,
    /// A response pair `"ANTE => CONS"` of a conjunction (repeatable; 0.5 merges
    /// `sv verify-liveness-all` into this verb).
    #[arg(
        long = "response",
        value_name = "ANTE => CONS",
        conflicts_with = "fairness"
    )]
    responses: Vec<String>,
    /// A fairness atom assumed to hold infinitely often (repeatable; with `--request` /
    /// `--grant`): the property is decided under `⋀ GF fairᵢ`.
    #[arg(long = "fairness", value_name = "ATOM", requires = "request")]
    fairness: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
    #[command(flatten)]
    json: JsonArgs,
}

/// Arguments for `mununu sv verify-liveness --response` — SV-direct conjunction of
/// response-liveness properties.
#[derive(Args, Debug)]
struct SvVerifyLivenessAllArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    /// A response pair `"ANTE => CONS"` — both sides register-comparison atoms
    /// (`"req == 1 => grant == 1"`). Repeatable; the verdict is the conjunction
    /// `⋀ AG(ANTE → AF CONS)`. At least one required.
    #[arg(long = "response", value_name = "ANTE => CONS", required = true)]
    responses: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
    #[command(flatten)]
    json: JsonArgs,
}

/// Arguments for `mununu sv verify-recoverability` — SV-direct `AG EF good`.
#[derive(Args, Debug)]
struct SvVerifyRecoverabilityArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    /// The `good` atom to recover to — a register comparison, e.g. `"state_q == 3"`.
    #[arg(long, value_name = "ATOM")]
    target: String,
    /// Extra abstraction predicate(s) for the cube-path escalation,
    /// `[NAME:]REG == VALUE` (repeatable). Used only when the exact engine abstains; the
    /// escalation is automatic even with none.
    #[arg(long = "predicate", value_name = "[NAME:]REG == VALUE")]
    predicate: Vec<String>,
    /// Also emit a structured `refinement` alongside the verdict: a `vacuous` witness when the target
    /// is never reachable, an auto `config_partition` over the design's detected reset when recovery
    /// depends on it, and a best-effort "why ⊥ / what would decide it" hint. Diagnostic-only —
    /// never changes the canonical verdict.
    #[arg(long = "refine")]
    refine: bool,
    /// Assumption discovery (refined-verdicts capability B): when the property does NOT hold, search for
    /// an environment assumption φ (a single narrow input held at a value) under which it becomes a
    /// NON-VACUOUS HOLDS → the refinement reports `holds_under`. CONDITIONAL-only (never changes the
    /// canonical verdict). Implies the refined output; opt-in (it costs extra decide runs).
    #[arg(long = "discover-assumptions")]
    discover_assumptions: bool,
    /// Config-partition (refined-verdicts capability A): name config INPUTS to split the verdict over,
    /// each `NAME=v1,v2,...` (repeatable) → the refinement reports a `config_partition`, decided exactly
    /// per config. Implies the refined output. Best for a narrow / few-value config (cross-product capped).
    #[arg(long = "partition", value_name = "NAME=v1,v2,...")]
    partition: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
    #[command(flatten)]
    json: JsonArgs,
}

/// Arguments for `mununu sv check-fsm` — SV-direct auto illegal-encoding scan.
#[derive(Args, Debug)]
struct SvCheckFsmArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    /// Max state-register width to treat as an FSM (wider = datapath/counter, skipped).
    #[arg(long, value_name = "BITS", default_value_t = mununu_core::adapter::fsm_scan::DEFAULT_FSM_MAX_WIDTH)]
    max_width: u32,
    /// Also run the liveness inventory: legal encodings never reached, reachable encodings the
    /// register can never leave (absorbing), and whether it always returns to its reset value.
    /// Out of reset, resets pinned inactive. Report-only: the exit code is the encoding scan's.
    #[arg(long)]
    liveness: bool,
    /// An intended absorbing state (`REG=VALUE`, decimal or 0x-hex), listed as expected rather
    /// than absorbing. Repeatable; needs `--liveness`.
    #[arg(long, value_name = "REG=VALUE", requires = "liveness")]
    terminal: Vec<String>,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu sv lint` — the CI-time partial-write preflight.
#[derive(Args, Debug)]
struct SvLintArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    #[command(flatten)]
    ci: CiArgs,
}

/// Arguments for `mununu sv cdc-lint` — the structural clock-domain-crossing lint.
#[derive(Args, Debug)]
struct SvCdcLintArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    #[command(flatten)]
    ci: CiArgs,
    #[command(flatten)]
    json: JsonArgs,
}

/// Arguments for `mununu sv mutate` — apply a named structural mutation + re-verify.
#[derive(Args, Debug)]
struct SvMutateArgs {
    #[command(flatten)]
    lift: SvLiftArgs,
    /// The mutation to apply, one of four kinds (mununu#635 — all four have been
    /// applicable since #468; the help listed two): `stick:<reg>` (freeze a
    /// register), `drop-reset:<reg>` (remove a register's reset arm),
    /// `off-by-one:<reg>[@<const_nid>][:±1]` (shift a constant the register is
    /// compared against — the boundary fault), `invert-cond:<sig>` (negate a 1-bit
    /// signal). The `_` spellings `--list` prints as JSON keys (`off_by_one:…`) are
    /// accepted as aliases. Required unless `--list`.
    #[arg(
        long,
        value_name = "stick:REG|drop-reset:REG|off-by-one:REG[@NID][:±1]|invert-cond:SIG"
    )]
    mutation: Option<String>,
    /// Instead of mutating, list the available mutation TARGETS of the design, per
    /// kind: `stick` (every register), `drop_reset` (every reset-mux register),
    /// `off_by_one` (every register compared against a constant), `invert_cond`
    /// (every 1-bit signal). The keys are the kinds with `_`; `--mutation` takes
    /// either spelling.
    #[arg(long)]
    list: bool,
    /// Verification engine for the baseline + mutant runs (as `sv verify-auto`).
    #[arg(long, value_enum, default_value_t = EngineArg::PortfolioSequential)]
    engine: EngineArg,
    /// Disable reset-gating (keep the reset a free input). Default: reset-gated.
    #[arg(long)]
    no_gate_reset: bool,
    /// Must-edge inference — `smt-hyper-must` gives sound νμ verdicts (needed for a
    /// recoverability property to flip under `drop-reset`). Default `auto`
    /// (mununu#663): per-target must for Liveness/Mixed-class properties, may-only for the rest.
    #[arg(long, value_enum, default_value_t = MustEdgeInferenceArg::Auto)]
    must_edge_inference: MustEdgeInferenceArg,
    /// mununu#725 — config concretization, `SIGNAL=VALUE` (repeatable), as `sv
    /// verify-auto --pin`: pins a config input to a constant for the
    /// baseline and the mutant (e.g. `--pin rst_ni=1`). Mirrored on the API
    /// as `config_values`.
    #[arg(long = "pin", value_name = "SIGNAL=VALUE")]
    pin: Vec<String>,
    /// Fault-injection posture: a register the adversary may overwrite (`REG` or `REG:k`;
    /// `k` is the ONE global budget of injections, default 1 — a single-event upset;
    /// `REG:unbounded` lifts it). Repeatable; `gen[1].q` is read as `gen$1.q`. The
    /// corruption is a free value at a free cycle after `--fault-warmup`, gated
    /// structurally (no `constraint`), so every engine sees the same total model. A safety
    /// HOLDS transfers ("the countermeasure responds for every corruption at these sites
    /// within the budget"); a VIOLATED is a real escape under the declared sites and budget;
    /// an alternating (νμ) property over a corrupted register is skipped (the fault player
    /// could restore any state). Mirrored on the API as `faults`.
    #[arg(long = "fault", value_name = "REG[:k]")]
    fault: Vec<String>,
    /// Fault-injection posture from the design's own countermeasures: `sec-cm[:k]` selects,
    /// for every `$rose(X.err_o) |-> ##[0:n] alert` chain, the state cells `X` compares (the
    /// duplicated counters, the sparse-FSM state) as the sites, under one budget `k`
    /// (default 1). `sec-cm:0` selects without injecting and runs the antecedent-vacuity
    /// check only — the "before" instrument. Alternative to `--fault`. Mirrored on the API
    /// as `fault_model`.
    #[arg(long = "fault-model", value_name = "MODEL[:k]")]
    fault_model: Option<String>,
    /// Cycles before the first injection may fire (default 10, OpenTitan's own `[*10]`
    /// assumption). Mirrored on the API as `fault_warmup`.
    #[arg(long = "fault-warmup", value_name = "CYCLES", default_value_t = 10)]
    fault_warmup: u32,
    #[command(flatten)]
    ci: CiArgs,
}

#[derive(Args, Debug)]
struct TemplatesArgs {
    /// Filter templates by domain (rtl, agentic, software, synthesis, universal); the universal
    /// templates are always included.
    #[arg(long, value_name = "DOMAIN")]
    domain: Option<String>,
    /// A project template pack (`TemplateCatalog` JSON) layered over the built-in catalog and the
    /// deployment's packs. Repeatable.
    #[arg(long = "templates-file", value_name = "FILE")]
    templates_file: Vec<PathBuf>,
    /// Show details of a specific template by ID.
    #[arg(long, value_name = "ID")]
    id: Option<String>,
    /// Output as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum ContextCommand {
    /// Parse and validate main + sidecar documents, optionally copying them to an output folder.
    Merge(ContextMergeArgs),
    /// Emit a JSON summary of automata, predicates, controllers, and formulas.
    Summarize(ContextSummarizeArgs),
    /// List guard predicates registered for the context (optionally filter by automaton).
    Predicates(ContextPredicatesArgs),
    /// Evaluate a μ-calculus formula over the realised context.
    Eval(ContextEvalArgs),
    /// Synthesise a controller for the given automaton/formula pair.
    Synth(ContextSynthesizeArgs),
    /// Generate a Cytoscape graph visualization of automata.
    Graph(ContextGraphArgs),
}

#[derive(Args, Debug)]
struct ContextMergeArgs {
    /// Context + sidecar files to merge (first entry is treated as the main context).
    #[arg(value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,
    /// Directory where the provided files should be copied.
    #[arg(long = "output", value_name = "DIR")]
    output: Option<PathBuf>,
    /// Overwrite the output directory if it already exists.
    #[arg(long)]
    force: bool,
}

#[derive(Args, Debug)]
struct ContextSummarizeArgs {
    /// Primary context document to load.
    #[arg(value_name = "CONTEXT")]
    context: PathBuf,
    /// Optional sidecar documents to merge.
    #[arg(long = "sidecar", value_name = "FILE")]
    sidecars: Vec<PathBuf>,
    /// Translate from an external format before processing
    /// (tlsf, aiger, promela, xstate, sv-yosys, extraction, auto).
    #[arg(long = "adapter", value_name = "FORMAT")]
    adapter: Option<String>,
    /// Mode for extraction adapter: "fixed" or "vulnerable".
    #[arg(long = "mode", value_name = "MODE")]
    mode: Option<String>,
    /// Optional source-language preprocessor to run before adapter
    /// translation. Currently supports: `sv2v` (lowers SV2009/2012
    /// constructs to Verilog-2005 before Yosys; required for modern
    /// open-source RTL — Caliptra-RTL, OpenTitan, ibex, etc.).
    /// Only honoured by `--adapter sv-yosys`.
    #[arg(long = "preprocessor", value_name = "NAME")]
    preprocessor: Option<String>,
    /// Print the internal structure of the context to stdout or a file.
    #[arg(long = "print-structure", value_name = "FILE")]
    print_structure: Option<Option<PathBuf>>,
}

#[derive(Args, Debug)]
struct ContextPredicatesArgs {
    /// Primary context document to load.
    #[arg(value_name = "CONTEXT")]
    context: PathBuf,
    /// Optional sidecar documents to merge.
    #[arg(long = "sidecar", value_name = "FILE")]
    sidecars: Vec<PathBuf>,
    /// Translate from an external format before processing
    /// (tlsf, aiger, promela, xstate, sv-yosys, extraction, auto).
    #[arg(long = "adapter", value_name = "FORMAT")]
    adapter: Option<String>,
    /// Mode for extraction adapter: "fixed" or "vulnerable".
    #[arg(long = "mode", value_name = "MODE")]
    mode: Option<String>,
    /// Optional source-language preprocessor to run before adapter
    /// translation. Currently supports: `sv2v`. Only honoured by
    /// `--adapter sv-yosys`.
    #[arg(long = "preprocessor", value_name = "NAME")]
    preprocessor: Option<String>,
    /// Restrict output to a single automaton.
    #[arg(long = "automaton", value_name = "NAME")]
    automaton: Option<String>,
}

#[derive(Args, Debug)]
struct ContextEvalArgs {
    /// Primary context document to load.
    #[arg(value_name = "CONTEXT")]
    context: PathBuf,
    /// Optional sidecar documents to merge.
    #[arg(long = "sidecar", value_name = "FILE")]
    sidecars: Vec<PathBuf>,
    /// Translate from an external format before processing
    /// (tlsf, aiger, promela, xstate, sv-yosys, extraction, auto).
    #[arg(long = "adapter", value_name = "FORMAT")]
    adapter: Option<String>,
    /// Mode for extraction adapter: "fixed" or "vulnerable".
    #[arg(long = "mode", value_name = "MODE")]
    mode: Option<String>,
    /// Optional source-language preprocessor to run before adapter
    /// translation. Currently supports: `sv2v`. Only honoured by
    /// `--adapter sv-yosys`.
    #[arg(long = "preprocessor", value_name = "NAME")]
    preprocessor: Option<String>,
    /// μ-calculus formula to evaluate (by name from the context).
    #[arg(long = "formula", value_name = "NAME", conflicts_with = "template")]
    formula: Option<String>,
    /// Instantiate a property template instead of selecting an existing formula.
    #[arg(long = "template", value_name = "ID", conflicts_with = "formula")]
    template: Option<String>,
    /// Template argument bindings (KEY=VALUE). Repeatable.
    #[arg(long = "template-arg", value_name = "KEY=VALUE", requires = "template")]
    template_args: Vec<String>,
    /// Structural checks before any property: reachable deadlocks (with a path), unreachable
    /// declared states, labels that never fire, internal-only livelocks. With no `--formula` /
    /// `--template`, only the checks run.
    #[arg(long)]
    sanity: bool,
    /// A project template pack (`TemplateCatalog` JSON) layered over the built-in catalog and the
    /// deployment's packs. Repeatable.
    #[arg(long = "templates-file", value_name = "FILE")]
    templates_files: Vec<PathBuf>,
    /// Automaton over which the formula should be evaluated.
    #[arg(long = "automaton", value_name = "NAME")]
    automaton: String,
    /// Disable guard partitions during evaluation.
    #[arg(long = "no-partitions")]
    no_partitions: bool,
    /// Hide labels (comma-separated) — reclassify as internal before evaluation.
    #[arg(long = "hide", value_delimiter = ',')]
    hide: Vec<String>,
    /// Apply bisimulation minimization to the target automaton before evaluation.
    #[arg(long = "minimize")]
    minimize: bool,
    /// Stub .espec.json files to compose with the model (external library interfaces).
    #[arg(long = "stub", value_name = "FILE")]
    stubs: Vec<PathBuf>,
    /// Print the internal structure of the context to stdout or a file.
    #[arg(long = "print-structure", value_name = "FILE")]
    print_structure: Option<Option<PathBuf>>,
    /// Print the intermediate CTXDSL (after adapter translation) to stdout or a file.
    #[arg(long = "print-ctxdsl", value_name = "FILE")]
    print_ctxdsl: Option<Option<PathBuf>>,
    /// Print a per-property soundness summary after evaluation.
    #[arg(long = "soundness-report")]
    soundness_report: bool,
    /// The formula `holds` when every initial state satisfies it and is `violated` otherwise;
    /// `--sanity` findings count as `unknown`. Exit codes follow the shared gate.
    #[command(flatten)]
    ci: CiArgs,
    /// Print the verdict as JSON instead of text: the `ContextVerifyResponse` that
    /// `POST /api/v1/context/verify` returns (`outcome`, `results[]`, `sanity[]`).
    #[command(flatten)]
    json: JsonArgs,
}

#[derive(Args, Debug)]
struct ContextSynthesizeArgs {
    /// Primary context document to load.
    #[arg(value_name = "CONTEXT")]
    context: PathBuf,
    /// Optional sidecar documents to merge.
    #[arg(long = "sidecar", value_name = "FILE")]
    sidecars: Vec<PathBuf>,
    /// Translate from an external format before processing
    /// (tlsf, aiger, promela, xstate, sv-yosys, extraction, auto).
    #[arg(long = "adapter", value_name = "FORMAT")]
    adapter: Option<String>,
    /// Mode for extraction adapter: "fixed" or "vulnerable".
    #[arg(long = "mode", value_name = "MODE")]
    mode: Option<String>,
    /// Optional source-language preprocessor to run before adapter
    /// translation. Currently supports: `sv2v`. Only honoured by
    /// `--adapter sv-yosys`.
    #[arg(long = "preprocessor", value_name = "NAME")]
    preprocessor: Option<String>,
    /// μ-calculus formula to synthesise (by name from the context).
    #[arg(long = "formula", value_name = "NAME", conflicts_with = "template")]
    formula: Option<String>,
    /// Instantiate a property template instead of selecting an existing formula.
    #[arg(long = "template", value_name = "ID", conflicts_with = "formula")]
    template: Option<String>,
    /// Template argument bindings (KEY=VALUE). Repeatable.
    #[arg(long = "template-arg", value_name = "KEY=VALUE", requires = "template")]
    template_args: Vec<String>,
    /// A project template pack (`TemplateCatalog` JSON) layered over the built-in catalog and the
    /// deployment's packs. Repeatable.
    #[arg(long = "templates-file", value_name = "FILE")]
    templates_files: Vec<PathBuf>,
    /// Automaton over which the controller should be synthesised. (Ignored by
    /// `--controller-mode gr1`, which synthesises directly from the LTL spec —
    /// pass any placeholder there.)
    #[arg(long = "automaton", value_name = "NAME")]
    automaton: String,
    /// Disable guard partitions during evaluation.
    #[arg(long = "no-partitions")]
    no_partitions: bool,
    /// Run a structural minimisation pass on the synthesised controller.
    #[arg(long)]
    minimize: bool,
    /// Extract a positional strategy: keep only one controllable transition per state.
    /// Legacy flag — equivalent to `--mode functional`. When `--mode` is also
    /// provided, `--mode` wins.
    #[arg(long = "extract-strategy")]
    extract_strategy: bool,
    /// Controller extraction mode. Case-insensitive. One of:
    /// `projection` (default), `functional`, `permissive`,
    /// `signature-memory`, `product-game`, `parity-game`. Overrides
    /// `--extract-strategy` when set.
    #[arg(long = "controller-mode", value_name = "NAME")]
    controller_mode: Option<String>,
    /// Emit counterexample/counterstrategy diagnostics when unrealizable.
    #[arg(long)]
    counterexample: bool,
    /// Capture deadlock traces in diagnostics.
    #[arg(long = "deadlock-traces")]
    deadlock_traces: bool,
    /// Cap the number of counterstrategy traces collected.
    #[arg(long = "max-counter-traces", value_name = "N")]
    max_counter_traces: Option<usize>,
    /// Skip proof obligation emission for violating initial states.
    #[arg(long = "no-proof-obligations")]
    no_proof_obligations: bool,
    /// Path where a JSON summary of the synthesis result should be written.
    #[arg(long = "dump-json", value_name = "FILE")]
    dump_json: Option<PathBuf>,
    /// Path where a controller-only DSL snapshot should be written.
    #[arg(long = "emit-dsl", value_name = "FILE")]
    emit_dsl: Option<PathBuf>,
    /// Path where the synthesized GR(1) controller SystemVerilog should be
    /// written (only meaningful with `--controller-mode gr1`).
    #[arg(long = "emit-sv", value_name = "FILE")]
    emit_sv: Option<PathBuf>,
    /// Path where diagnostics should be exported as a DSL sidecar.
    #[arg(long = "dump-diagnostics", value_name = "FILE")]
    dump_diagnostics: Option<PathBuf>,
    /// Print the internal structure of the context to stdout or a file.
    #[arg(long = "print-structure", value_name = "FILE")]
    print_structure: Option<Option<PathBuf>>,
    /// Print the intermediate CTXDSL (after adapter translation) to stdout or a file.
    #[arg(long = "print-ctxdsl", value_name = "FILE")]
    print_ctxdsl: Option<Option<PathBuf>>,
    /// Output format for the synthesized controller: ctxdsl (default), xstate, systemverilog.
    #[arg(long = "output-format", value_name = "FORMAT")]
    output_format: Option<String>,
    /// Path where the native-format controller should be written (requires --output-format).
    #[arg(long = "emit-native", value_name = "FILE")]
    emit_native: Option<PathBuf>,
    /// Print a per-property soundness summary after synthesis.
    #[arg(long = "soundness-report")]
    soundness_report: bool,
}

#[derive(Args, Debug)]
struct ContextGraphArgs {
    /// Primary context document to load.
    #[arg(value_name = "CONTEXT")]
    context: PathBuf,
    /// Optional sidecar documents to merge.
    #[arg(long = "sidecar", value_name = "FILE")]
    sidecars: Vec<PathBuf>,
    /// Type of graph to generate: `dsl` (ctxdsl inferred automata), `unrolled` (internal after unrolling), or `both`.
    #[arg(long, value_enum, default_value = "dsl")]
    r#type: GraphOutputType,
    /// Output file path for the HTML visualization.
    #[arg(long = "output", value_name = "FILE")]
    output: PathBuf,
    /// Restrict output to a single automaton.
    #[arg(long = "automaton", value_name = "NAME")]
    automaton: Option<String>,
    /// Generate a counterstrategy graph for a failed formula (requires --formula and --automaton).
    #[arg(long)]
    counterstrategy: bool,
    /// Formula to evaluate for counterstrategy generation.
    #[arg(long = "formula", value_name = "NAME")]
    formula: Option<String>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum GraphOutputType {
    /// Show only the ctxdsl inferred automata graph.
    Dsl,
    /// Show only the internal representation after abstraction/unrolling.
    Unrolled,
    /// Show both graphs.
    Both,
}

fn main() {
    // mununu#715 — every report this process produces (CLI `--json`, `mununu server`) carries the
    // identity of this binary.
    mununu_core::build_info::register(mununu_core::build_info::identity_from(
        option_env!("MUNUNU_BUILD_SHA"),
        option_env!("MUNUNU_BUILD_DIRTY"),
    ));
    // Export our own path so the reachability portfolio can run z3-SPACER in an
    // ISOLATED child (`btor2 spacer-check`) — z3's Fixedpoint can flaky-segfault on some
    // CHC encodings, and isolating it keeps a crash from taking down the whole verify.
    // Set once here, before any threads, so the later concurrent reads are race-free.
    if let Ok(exe) = std::env::current_exe() {
        // SAFETY: single-threaded at process start; no other thread reads/writes env yet.
        unsafe { std::env::set_var("MUNUNU_SELF_EXE", exe) };
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            std::process::exit(parse_error_exit_code(&e));
        }
    };
    init_tracing(cli.quiet);
    let server_config = match &cli.command {
        Commands::Server { config, .. } => config.clone(),
        _ => None,
    };
    if let Some(path) = server_config.or(cli.deployment_config.clone()) {
        let installed = mununu_core::deployment::DeploymentConfig::load(&path)
            .and_then(mununu_core::deployment::install);
        if let Err(err) = installed {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
    if let Err(err) = dispatch(cli.command) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

/// Resolve the controller mode from the CLI flags.
///
/// Precedence:
/// 1. `--mode <name>` (if `Some`) is parsed case-insensitively.
/// 2. Else, `--extract-strategy` → `Functional` (legacy mapping).
/// 3. Else, `Projection` (default).
fn parse_cli_controller_mode(
    mode: &Option<String>,
    extract_strategy: bool,
) -> Result<mununu_core::context::ControllerMode, String> {
    use mununu_core::context::ControllerMode;
    if let Some(name) = mode {
        return ControllerMode::from_normalized_name(name).map_err(|other| {
            format!(
                "unknown controller mode '{other}' \
                (valid: projection, functional, permissive, signature-memory, product-game, parity-game, gr1)"
            )
        });
    }
    Ok(if extract_strategy {
        ControllerMode::Functional
    } else {
        ControllerMode::Projection
    })
}

#[cfg(test)]
mod sv_frontend_arg_tests {
    use super::*;
    use mununu_core::adapter::yosys::SvFrontend;

    // Guards against a copy-paste swap in the CLI → core front-end mapping
    // (e.g. Slang accidentally routed to Verilog would silently disable the
    // whole slang-lift capability while still reporting "frontend=slang").
    #[test]
    fn frontend_arg_maps_to_core_variant() {
        assert_eq!(SvFrontend::from(SvFrontendArg::Auto), SvFrontend::Auto);
        assert_eq!(
            SvFrontend::from(SvFrontendArg::Verilog),
            SvFrontend::Verilog
        );
        assert_eq!(SvFrontend::from(SvFrontendArg::Slang), SvFrontend::Slang);
    }

    #[test]
    fn frontend_arg_default_is_auto() {
        assert!(matches!(SvFrontendArg::default(), SvFrontendArg::Auto));
    }
}

#[cfg(test)]
mod cli_controller_mode_tests {
    use super::*;
    use mununu_core::context::ControllerMode;

    #[test]
    fn default_is_projection() {
        assert_eq!(
            parse_cli_controller_mode(&None, false).unwrap(),
            ControllerMode::Projection
        );
    }

    #[test]
    fn extract_strategy_maps_to_functional() {
        assert_eq!(
            parse_cli_controller_mode(&None, true).unwrap(),
            ControllerMode::Functional
        );
    }

    #[test]
    fn explicit_mode_wins_over_extract_strategy() {
        assert_eq!(
            parse_cli_controller_mode(&Some("projection".into()), true).unwrap(),
            ControllerMode::Projection
        );
    }

    #[test]
    fn parses_all_modes() {
        let cases = [
            ("projection", ControllerMode::Projection),
            ("functional", ControllerMode::Functional),
            ("permissive", ControllerMode::Permissive),
            ("signature-memory", ControllerMode::SignatureMemory),
            ("Signature_Memory", ControllerMode::SignatureMemory),
            ("product-game", ControllerMode::ProductGame),
            ("PARITY-GAME", ControllerMode::ParityGame),
            ("paritygame", ControllerMode::ParityGame),
        ];
        for (name, expected) in cases {
            assert_eq!(
                parse_cli_controller_mode(&Some(name.into()), false).unwrap(),
                expected,
                "name `{name}`"
            );
        }
    }

    #[test]
    fn rejects_unknown_mode() {
        let err = parse_cli_controller_mode(&Some("strict".into()), false).unwrap_err();
        assert!(err.contains("unknown controller mode"));
        assert!(err.contains("strict"));
    }
}

fn init_tracing(quiet: bool) {
    // CI mode (`--quiet`): no `logs/` file in the workspace, no init banner, errors
    // only to stderr. `RUST_LOG` still wins if the user sets it explicitly.
    if quiet {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error"));
        let _ = fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(false)
            .compact()
            .try_init();
        return;
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Create logs directory if it doesn't exist
    let logs_dir = PathBuf::from("logs");
    let log_file = logs_dir.join("mununu.log");

    // Try to set up file logging
    let file_result = fs::create_dir_all(&logs_dir)
        .and_then(|_| File::options().create(true).append(true).open(&log_file));

    match file_result {
        Ok(file) => {
            // Set up dual logging: both stdout and file
            let file_layer = fmt::layer()
                .with_writer(file)
                .with_ansi(false) // Disable ANSI colors in file
                .with_target(false)
                .compact();

            let stderr_layer = fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .compact();

            // Initialize with both layers and apply filter at the registry level
            let _ = Registry::default()
                .with(filter)
                .with(file_layer)
                .with(stderr_layer)
                .try_init();

            tracing::info!(
                log_file = %log_file.display(),
                "Logging initialized. Logs are written to both stderr and file."
            );
        }
        Err(e) => {
            eprintln!(
                "Warning: Failed to set up file logging ({:?}): {}. Logs will only go to stderr.",
                log_file, e
            );
            // Fall back to STDERR-only logging. `fmt()`'s default writer is STDOUT, which would
            // pollute the JSON result stream (`mununu … | jq` breaks) whenever the `logs/` dir is
            // unwritable (read-only CWD / sandbox / CI). stdout is reserved for the command's data.
            let builder = fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(false)
                .compact();
            let _ = builder.try_init();
        }
    }
}

fn handle_verify(args: VerifyArgs) -> Result<(), String> {
    use mununu_core::verify::config::VerifyConfig;
    use mununu_core::verify::report::PropertyFormulaSource;
    use mununu_core::verify::{inspect_project, verify_project};

    let body = std::fs::read_to_string(&args.config)
        .map_err(|e| format!("failed to read {}: {e}", args.config.display()))?;
    let mut config = VerifyConfig::from_toml(&body)
        .map_err(|e| format!("failed to parse {} as TOML: {e}", args.config.display()))?;

    // R4W-3 — the CLI flag overrides any `cluster_similarity_floor` set
    // in the verify.toml; absent flag leaves the manifest value (or its
    // None default) in place.
    if args.cluster_coi_floor.is_some() {
        config.cluster_similarity_floor = args.cluster_coi_floor;
    }
    // mununu#594 — likewise for the counterexample step cap.
    if args.counterexample_max_steps.is_some() {
        config.counterexample_max_steps = args.counterexample_max_steps;
    }

    let base_dir = args.base_dir.clone().unwrap_or_else(|| {
        args.config
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });

    if args.print_alphabet {
        let inspection = inspect_project(&config, &base_dir).map_err(|e| format!("{e}"))?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&inspection).map_err(|e| e.to_string())?
            );
        } else {
            print_inspection_human(&inspection);
        }
        return Ok(());
    }

    let report = verify_project(&config, &base_dir).map_err(|e| format!("{e}"))?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!("verify report — project `{}`:", report.project);
        println!(
            "  composition: {} {} {{ members = [{}] }}",
            report.composition.semantics,
            report.composition.name,
            report.composition.members.join(", "),
        );
        println!("  sources:");
        for s in &report.sources {
            println!(
                "    - {id} (adapter = {adapter}, automaton = {automaton})",
                id = s.id,
                adapter = s.adapter,
                automaton = s.automaton.as_deref().unwrap_or("(unresolved)"),
            );
            // R4W-3 — clustered cone-of-influence comparison (BTOR2 /
            // `sv-yosys` route with declared properties). Present only
            // when the bit-blaster computed it; absent for other
            // adapters or property-less sources.
            if let Some(cc) = s
                .partition_summary
                .as_ref()
                .and_then(|ps| ps.cluster_coi.as_ref())
            {
                println!(
                    "        clustered-COI: joint cone {joint} signals, {n} cluster(s), max cluster cone {max} signals{verdict}",
                    joint = cc.joint_cone_size,
                    n = cc.clusters.len(),
                    max = cc.max_cluster_cone_size,
                    verdict = if cc.max_cluster_cone_size < cc.joint_cone_size {
                        format!(
                            " (reduces binding cone by {} vs joint COI)",
                            cc.joint_cone_size - cc.max_cluster_cone_size
                        )
                    } else {
                        " (no reduction — cones overlap or single cluster)".to_string()
                    },
                );
            }
            // R46-4 (R.4.6) — per-cluster verification mode. Present only
            // when the joint design busted the state-bit cap and the
            // bit-blaster fell back to verifying each cluster separately
            // (`PartitionSummary::cluster_routing`). Each property's
            // per-cluster automaton is also visible in its `over` field
            // below; this line surfaces the mode + the distinct cluster
            // count up front.
            if let Some(routing) = s
                .partition_summary
                .as_ref()
                .and_then(|ps| ps.cluster_routing.as_ref())
            {
                let n_clusters = routing
                    .values()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                println!(
                    "        per-cluster verification: joint design exceeded the state-bit cap → \
                     {n_clusters} cluster(s) verified separately ({} property route(s))",
                    routing.len(),
                );
            }
        }
        println!("  properties ({}):", report.property_verdicts.len());
        for v in &report.property_verdicts {
            let source_str = match &v.formula_source {
                PropertyFormulaSource::Inline => "inline".to_string(),
                PropertyFormulaSource::Template { id, .. } => format!("template `{id}`"),
            };
            // mununu#595 — a conditional verdict is never a bare SATISFIED: it says what it
            // holds UNDER, and when the assumptions admit no path at all it says VACUOUS
            // instead, because the formula is then true for nothing.
            let verdict = match (v.satisfied(), v.fair_path_exists) {
                (true, Some(false)) => format!(
                    "VACUOUS under {{{}}} — the assumptions admit no fair path from the initial \
                     state(s); the formula is trivially true",
                    v.assumptions.join(", ")
                ),
                (true, Some(true)) => format!("SATISFIED under {{{}}}", v.assumptions.join(", ")),
                (false, Some(_)) => format!("VIOLATED under {{{}}}", v.assumptions.join(", ")),
                (true, None) => "SATISFIED".to_string(),
                (false, None) => "VIOLATED".to_string(),
            };
            println!(
                "    {name}: {verdict} ({sat}/{total} states, {init_sat}/{init} initial) [{source}, over = {over}]",
                name = v.name,
                verdict = verdict,
                sat = v.satisfying_states,
                total = v.total_states,
                init_sat = v.initial_satisfying.len(),
                init = v.initial_states.len(),
                source = source_str,
                over = v.over,
            );
            if args.print_counterexample
                && let Some(witness) = v.counterexample.as_ref()
            {
                println!("      counterexample (from {}):", witness.initial_state);
                for (i, step) in witness.steps.iter().enumerate() {
                    println!(
                        "        {idx}. --[{label}]--> {state}",
                        idx = i + 1,
                        label = step.label,
                        state = step.successor_state,
                    );
                }
                let term = match &witness.termination {
                    mununu_core::verify::report::TraceTermination::Sink => "sink".to_string(),
                    mununu_core::verify::report::TraceTermination::Cycle { return_to_step } => {
                        format!("cycle back to step {return_to_step}")
                    }
                    mununu_core::verify::report::TraceTermination::LengthLimit => {
                        format!(
                            "length-limit (truncated at {} steps; raise with \
                             --counterexample-max-steps)",
                            report.counterexample_max_steps
                        )
                    }
                };
                // mununu#594 — on the safety shape the witness names the `!phi` state its
                // shortest path leads to, so a truncated trace still says where it was going.
                match &witness.violating_state {
                    Some(target) if witness.steps.len() < report.counterexample_max_steps => {
                        println!("        ({term}; violating state: {target})");
                    }
                    Some(target) => {
                        println!("        ({term}; leads to violating state: {target})");
                    }
                    None => println!("        ({term})"),
                }
            }
        }
        if !report.safety_cube_results.is_empty() {
            println!(
                "  safety-cube AG !bad ({}):",
                report.safety_cube_results.len()
            );
            for r in &report.safety_cube_results {
                let label = match &r.property {
                    Some(p) => format!("{}::{p}", r.source_id),
                    None => r.source_id.clone(),
                };
                println!(
                    "    {label}: {verdict} [{file}]",
                    verdict = r.verdict,
                    file = r.file,
                );
            }
        }
    }

    // mununu#595 — a vacuous conditional verdict is not a pass either: it reads `unknown`.
    let worst = if report.property_verdicts.iter().any(|v| !v.satisfied()) {
        "violated"
    } else if report
        .property_verdicts
        .iter()
        .any(|v| v.fair_path_exists == Some(false))
    {
        "unknown"
    } else {
        "holds"
    };
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

fn handle_memory(command: MemoryCommand) -> Result<(), String> {
    match command {
        MemoryCommand::Check(args) => handle_memory_check(args),
    }
}

fn handle_memory_check(args: MemoryCheckArgs) -> Result<(), String> {
    use mununu_core::verify::config::VerifyConfig;
    use mununu_core::verify::memory_check::check_memory_postures;

    let body = std::fs::read_to_string(&args.config)
        .map_err(|e| format!("failed to read {}: {e}", args.config.display()))?;
    let config = VerifyConfig::from_toml(&body)
        .map_err(|e| format!("failed to parse {} as TOML: {e}", args.config.display()))?;
    let report = check_memory_postures(&config);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.format_human());
    }

    if args.strict && report.has_warnings() {
        return Err(format!(
            "{} warning(s) raised; --strict requested",
            report.warnings.len()
        ));
    }
    Ok(())
}

fn print_inspection_human(report: &mununu_core::verify::InspectionReport) {
    println!("inspect report — project `{}`:", report.project);
    println!(
        "  composition: {} {} {{ members = [{}] }}",
        report.composition.info.semantics,
        report.composition.info.name,
        report.composition.info.members.join(", "),
    );
    println!("  sources:");
    for s in &report.sources {
        println!(
            "    - {id} (adapter = {adapter}, automaton = {automaton})",
            id = s.id,
            adapter = s.adapter,
            automaton = s.automaton.as_deref().unwrap_or("(unresolved)"),
        );
    }
    println!("  automata ({}):", report.automata.len());
    for a in &report.automata {
        let src = a.source_id.as_deref().unwrap_or("-");
        println!(
            "    {name} (source = {src}, {n_states} states, {n_init} initial, {n_alpha} labels)",
            name = a.name,
            n_states = a.states.len(),
            n_init = a.initial_states.len(),
            n_alpha = a.alphabet.len(),
        );
        if !a.initial_states.is_empty() {
            println!("      initial: {}", a.initial_states.join(", "));
        }
        if !a.states.is_empty() {
            println!("      states:  {}", a.states.join(", "));
        }
        if !a.alphabet.is_empty() {
            println!("      labels:  {}", a.alphabet.join(", "));
        }
    }
    println!();
    println!(
        "  composition alphabet ({} labels):",
        report.composition.alphabet.len()
    );
    if !report.composition.alphabet.is_empty() {
        for chunk in report.composition.alphabet.chunks(4) {
            println!("    {}", chunk.join(", "));
        }
    }
    if !report.composition.state_names.is_empty() {
        println!(
            "  composition states ({}):",
            report.composition.state_names.len()
        );
        for chunk in report.composition.state_names.chunks(4) {
            println!("    {}", chunk.join(", "));
        }
    }
    if !report.composition.predicate_names.is_empty() {
        println!(
            "  declared predicates ({}):",
            report.composition.predicate_names.len()
        );
        for chunk in report.composition.predicate_names.chunks(4) {
            println!("    {}", chunk.join(", "));
        }
    }
}

fn dispatch(command: Commands) -> Result<(), String> {
    match command {
        Commands::InternalEngineEval(args) => internal_engine_eval(args),
        Commands::Context { command } => handle_context(*command),
        Commands::Extraction { command } => handle_extraction(*command),
        Commands::Sv { command } => handle_sv(*command),
        Commands::Templates(args) => list_templates(args),
        Commands::Library { command } => handle_library(*command),
        Commands::Contract { command } => handle_contract(*command),
        Commands::Codesign { command } => handle_codesign(*command),
        Commands::Verify(args) => handle_verify(args),
        Commands::Memory { command } => handle_memory(*command),
        Commands::Server { addr, .. } => {
            use std::net::SocketAddr;
            let addr: SocketAddr = addr
                .parse()
                .map_err(|e| format!("Invalid address: {}", e))?;
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to create runtime: {}", e))?;
            rt.block_on(mununu_core::api::start_server(addr))
                .map_err(|e| format!("Server error: {}", e))?;
            Ok(())
        }
        Commands::Btor2 { command } => handle_btor2(*command),
    }
}

fn btor2_lift_kmts(args: Btor2LiftKmtsArgs) -> Result<(), String> {
    use mununu_core::adapter::AdapterOptions;
    use mununu_core::adapter::btor2::{KmtsLiftOptions, lift_btor2_to_kmts};

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let opts = AdapterOptions::default();
    let lift_opts = KmtsLiftOptions {
        max_predicates: args.max_predicates,
        ..Default::default()
    };
    let result = lift_btor2_to_kmts(&content, &opts, &lift_opts)
        .map_err(|e| format!("btor2 lift-kmts: {}", e.message))?;

    let summary = serde_json::json!({
        "fixture": args.file.display().to_string(),
        "predicates_synthesised": result.predicates.len(),
        "labelling_count": result.labelling_count(),
        "automata": result.predicate_labellings.keys().collect::<Vec<_>>(),
        "predicate_names": result.predicates.iter().map(|p| &p.name).collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    Ok(())
}

fn handle_btor2(command: Btor2Command) -> Result<(), String> {
    match command {
        Btor2Command::Discover(args) => btor2_discover(args),
        Btor2Command::LiftKmts(args) => btor2_lift_kmts(args),
        Btor2Command::Cegar(args) => btor2_cegar(args),
        Btor2Command::Verify(args) => btor2_verify(args),
        Btor2Command::InjectFault(args) => btor2_inject_fault(args),
        Btor2Command::SpacerCheck(args) => btor2_spacer_check(args),
        Btor2Command::VerifyLiveness(args) => btor2_verify_liveness_merged(args),
        Btor2Command::FromAiger(args) => btor2_from_aiger(args),
        Btor2Command::VerifyRecoverability(args) => btor2_verify_recoverability(args),
        Btor2Command::CheckFsm(args) => btor2_check_fsm(args),
        Btor2Command::Game(args) => btor2_game(args),
    }
}

/// `mununu btor2 check-fsm` — auto-scan FSM-like state registers for unrecoverable
/// traps (no user input) and print the per-register recoverability result as JSON.
/// Exits non-zero on any trap (via `--fail-on`).
fn btor2_check_fsm(args: Btor2CheckFsmArgs) -> Result<(), String> {
    use mununu_core::adapter::fsm_scan::fsm_encoding_scan;
    use mununu_core::verdict::PropertyVerdict;

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let findings = fsm_encoding_scan(&content, args.max_width)?;

    let registers: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "register": f.register,
                "kind": f.kind.as_str(),
                "legal_encodings": f.legal_encodings,
                "outcome": f.verdict.as_str(),
                "illegal_encoding_reachable": f.is_finding(),
            })
        })
        .collect();
    let mut summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "fsm_registers_checked": findings.iter().filter(|f| f.is_checked()).count(),
        "illegal_encodings_found": findings.iter().filter(|f| f.is_finding()).count(),
        "registers": registers,
    });
    if args.liveness {
        let terminal = mununu_core::api::models::parse_terminal_states(&args.terminal)?;
        let inv =
            mununu_core::adapter::fsm_scan::fsm_liveness_scan(&content, args.max_width, &terminal)?;
        summary["liveness"] = liveness_json(&inv);
    }
    print_json_summary(&summary)?;

    // Exit code: the worst verdict across the checked registers.
    let worst = worst_verdict(findings.iter().map(|f| PropertyVerdict::as_str(f.verdict)));
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

/// The liveness inventory as JSON — the same shape as the API's `FsmLivenessView`.
fn liveness_json(inv: &[mununu_core::adapter::fsm_scan::FsmLivenessFinding]) -> serde_json::Value {
    serde_json::to_value(
        inv.iter()
            .map(mununu_core::api::models::FsmLivenessView::from)
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default()
}

/// `mununu btor2 game` — solve the two-player controllable-reachability game and synthesize the winner's
/// strategy (the controller's Mealy strategy, or the environment's positional counterstrategy). Surface
/// peer of the API `POST /api/v1/btor2/game`. Exits non-zero (via `--fail-on`) on `unrealizable` (mapped
/// to the `violated` verdict class); `realizable` maps to `holds`.
fn btor2_game(args: Btor2GameArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::symbolic_bitblast::{
        exact_two_player_buchi_realizable, exact_two_player_buchi_strategy,
        exact_two_player_reach_realizable, exact_two_player_recurrence_stall_lasso,
        exact_two_player_strategy, game_sound_posture_model,
    };

    let raw = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    // --assume-clock-reset: model clk/rst as a sound posture (not adversarial) before solving the game.
    let content = if args.assume_clock_reset {
        game_sound_posture_model(&raw)
    } else {
        raw
    };
    // `--fault`: inject AFTER the posture (the fault mux outermost on the final `next`; the warm-up
    // counts from the posture's init); the fault inputs join the controllable side.
    let (content, fault_inputs, fault_label) = if args.fault.is_empty() {
        (content, Vec::new(), None)
    } else {
        use mununu_core::adapter::btor2::fault::{FaultSpec, inject_faults};
        let spec = FaultSpec::parse_many(&args.fault, args.fault_warmup)?;
        let file = mununu_core::adapter::btor2::parser::parse(&content)
            .map_err(|e| format!("BTOR2 parse error: {e}"))?;
        let (out, _) = inject_faults(&file, &spec).map_err(|m| format!("--fault: {m}"))?;
        (
            mununu_core::adapter::btor2::emit::emit_btor2(&out),
            spec.input_names(),
            Some(spec.as_label()),
        )
    };
    let controllable: Vec<&str> = args
        .controllable
        .iter()
        .map(String::as_str)
        .chain(fault_inputs.iter().map(String::as_str))
        .collect();
    let recurrence = args.objective == GameObjective::Recurrence;
    // The VERDICT works on any resolvable target (state cell, combinational output, relation) for both
    // objectives; it also validates the controllable partition. `reach` = force `good` once; `recurrence`
    // = force `good` infinitely often (Büchi).
    let realizable = if recurrence {
        exact_two_player_buchi_realizable(&content, &args.good, &controllable)?
    } else {
        exact_two_player_reach_realizable(&content, &args.good, &controllable)?
    };
    // STRATEGY extraction — the WINNER's strategy, best-effort (needs a STATE-register target, so it is
    // omitted for a combinational-output / relational `good`). `reach` = the reachability attractor
    // strategy (controller Mealy, or the env positional counterstrategy when unrealizable); `recurrence`
    // = the CONTROLLER's Büchi strategy when realizable (`None` when unrealizable — the env starvation
    // lasso below is that case's witness).
    // A strategy that could not be extracted is reported (`strategy_error`), never dropped silently.
    let (strategy, strategy_error) = if recurrence {
        match exact_two_player_buchi_strategy(&content, &args.good, &controllable) {
            Ok(s) => (s, None),
            Err(e) => (None, Some(e)),
        }
    } else {
        match exact_two_player_strategy(&content, &args.good, &controllable) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e)),
        }
    };

    let (outcome, detail) =
        mununu_core::api::models::game_outcome(fault_label.is_some(), realizable);
    let mut summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "outcome": outcome,
        "detail": detail,
        "good": args.good,
        "objective": if recurrence { "recurrence" } else { "reach" },
        "controllable": args.controllable,
        "assume_clock_reset": args.assume_clock_reset,
        "realizable": realizable,
    });
    if let Some(e) = &strategy_error {
        summary["strategy_error"] = serde_json::Value::String(e.clone());
    }
    if let Some(label) = &fault_label {
        summary["fault"] = serde_json::Value::String(label.clone());
        summary["attack_exists"] = serde_json::Value::Bool(realizable);
    }
    if let Some(s) = &strategy {
        summary["strategy"] =
            serde_json::to_value(s).map_err(|e| format!("serialize strategy: {e}"))?;
    }
    // ENVIRONMENT STARVATION LASSO — the actionable witness for an unrealizable RECURRENCE game: a
    // concrete play (reset → `¬good` cycle, with the env's per-step inputs) proving the environment can
    // starve `good` forever. REACH failures already carry the env positional counterstrategy (in
    // `strategy`); this is the Büchi analog. Best-effort — omitted when there is no simple reachable
    // force-`¬good`-forever region (a subtler co-Büchi); the `realizable=false` verdict still stands.
    if recurrence
        && !realizable
        && let Some(lasso) =
            exact_two_player_recurrence_stall_lasso(&content, &args.good, &controllable)?
    {
        summary["trace"] =
            serde_json::to_value(mununu_core::api::models::Trace::from_stall_lasso(&lasso))
                .map_err(|e| format!("serialize stall_lasso: {e}"))?;
    }
    // --discover-assumptions: when the game is unrealizable, search for an environment assumption under
    // which the controller wins (CONDITIONAL — never flips `realizable`). For `reach` this is a SAFETY
    // hold / conjunction (`A ⇒ G`); for `recurrence` it is a FAIRNESS assumption `GF a → GF good` (the
    // GR(1) 1-pair objective). No-op when the game is already realizable.
    if args.discover_assumptions {
        let holds_under = if recurrence {
            mununu_core::adapter::recoverability::discover_game_fairness_assumption(
                &content,
                &args.good,
                &controllable,
            )
        } else {
            mununu_core::adapter::recoverability::discover_game_env_assumption(
                &content,
                &args.good,
                &controllable,
            )
        };
        if !holds_under.is_empty() {
            summary["holds_under"] = serde_json::to_value(&holds_under)
                .map_err(|e| format!("serialize holds_under: {e}"))?;
        }
    }
    print_json_summary(&summary)?;

    ci_gate_exit(outcome, args.ci.fail_on);
    Ok(())
}

/// `mununu btor2 verify-recoverability` — decide `AG EF target` via the exact
/// 3-valued engine and print the canonical verdict as JSON. Surface peer of the API
/// `POST /api/v1/btor2/verify-recoverability`.
fn btor2_verify_recoverability(args: Btor2VerifyRecoverabilityArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::symbolic_bitblast::ExactSymbolicOptions;
    use mununu_core::adapter::recoverability::{
        parse_config_value_specs, parse_extra_predicate, recoverability_property_str,
        verify_recoverability_explained, verify_recoverability_refined_explained,
    };

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let extra = args
        .predicate
        .iter()
        .map(|s| parse_extra_predicate(s))
        .collect::<Result<Vec<_>, _>>()?;
    let config_specs = parse_config_value_specs(&args.partition)?;

    let mut summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "property": recoverability_property_str(&args.target),
    });
    // `--refine` / `--partition` / `--discover-assumptions` (refined-verdicts): the canonical
    // verdict PLUS a structured, diagnostic-only refinement (Vacuous / bot-diagnosis / config-partition
    // / holds_under) — never changes the verdict. Without any, the plain verdict path.
    let (verdict, attribution) =
        if args.refine || !config_specs.is_empty() || args.discover_assumptions {
            let (verdict, refinement, attribution) = verify_recoverability_refined_explained(
                &content,
                &args.target,
                &extra,
                &config_specs,
                args.discover_assumptions,
            );
            summary["refinement"] = serde_json::to_value(&refinement)
                .map_err(|e| format!("serialize refinement: {e}"))?;
            (verdict, attribution)
        } else {
            let (verdict, attribution) = verify_recoverability_explained(
                &content,
                &args.target,
                &extra,
                &ExactSymbolicOptions::default(),
            )?;
            (verdict, Some(attribution))
        };
    summary["outcome"] = serde_json::Value::String(verdict.as_str().to_string());
    // Who decided, and why the exact engine stepped aside when the cube ladder did — the same two
    // keys the API response carries (`engine`, `exact_abstention`).
    if let Some(a) = &attribution {
        summary["engine"] = serde_json::Value::String(a.engine.as_str().to_string());
        if let Some(kind) = a.exact_abstention {
            summary["exact_abstention"] = serde_json::Value::String(kind.label().to_string());
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    ci_gate_exit(verdict.as_str(), args.ci.fail_on);
    Ok(())
}

/// `mununu btor2 verify-liveness` — decide the response-liveness property
/// `AG(request → AF grant)` via the l2s reduction + the portfolio, printing the
/// verdict as JSON. Surface peer of the API `POST /api/v1/btor2/verify-liveness`;
/// both share the canonical verdict label via
/// [`mununu_core::verdict::PropertyVerdict`].
/// Read a design that is BTOR2 text or AIGER (binary / ASCII); AIGER is lowered to BTOR2.
fn read_btor2_or_aiger(path: &std::path::Path) -> Result<(String, bool), String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;
    if bytes.starts_with(b"aig ") || bytes.starts_with(b"aag ") {
        let circuit = mununu_core::adapter::aiger::parser::parse_bytes(&bytes)
            .map_err(|e| format!("AIGER '{}': {}", path.display(), e.message))?;
        Ok((
            mununu_core::adapter::aiger::to_btor2::circuit_to_btor2(&circuit),
            true,
        ))
    } else {
        Ok((
            String::from_utf8(bytes).map_err(|e| {
                format!("'{}' is neither AIGER nor UTF-8 BTOR2: {e}", path.display())
            })?,
            false,
        ))
    }
}

fn btor2_from_aiger(args: Btor2FromAigerArgs) -> Result<(), String> {
    let (btor2, was_aiger) = read_btor2_or_aiger(&args.file)?;
    if !was_aiger {
        return Err(format!(
            "'{}' does not start with an `aig`/`aag` header",
            args.file.display()
        ));
    }
    match args.output {
        Some(p) => std::fs::write(&p, btor2).map_err(|e| format!("write '{}': {e}", p.display())),
        None => {
            print!("{btor2}");
            Ok(())
        }
    }
}

fn btor2_verify_justice(args: Btor2VerifyJusticeArgs) -> Result<(), String> {
    use mununu_core::adapter::justice::{JusticeOptions, JusticeRung, verify_justice_with};
    let rung = JusticeRung::parse(&args.rung)?;
    let (btor2, converted) = read_btor2_or_aiger(&args.file)?;
    let t0 = std::time::Instant::now();
    let results = verify_justice_with(
        &btor2,
        &JusticeOptions {
            rung,
            budget_ms: args.timeout_ms,
            parallel_rungs: args.parallel_rungs,
        },
    )?;
    let elapsed_ms = t0.elapsed().as_millis() as u64;
    let worst = if results
        .iter()
        .any(|r| r.verdict == mununu_core::verdict::PropertyVerdict::Violated)
    {
        "violated"
    } else if results
        .iter()
        .any(|r| r.verdict == mununu_core::verdict::PropertyVerdict::Unknown)
    {
        "unknown"
    } else {
        "holds"
    };
    let summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "converted_from_aiger": converted,
        "rung": args.rung,
        "elapsed_ms": elapsed_ms,
        "outcome": worst,
        "justice": results.iter().map(|r| serde_json::json!({
            "index": r.index,
            "size": r.size,
            "outcome": r.verdict.as_str(),
            "decided_by": r.decided_by,
            "notes": r.notes,
        })).collect::<Vec<_>>(),
    });
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&summary).map_err(|e| format!("serialize: {e}"))?
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize: {e}"))?
        );
    }
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

/// `mununu btor2 verify-liveness` — dispatch the merged verb to the response, conjunction,
/// fairness or justice form.
fn btor2_verify_liveness_merged(a: Btor2VerifyLivenessArgs) -> Result<(), String> {
    if a.justice {
        return btor2_verify_justice(Btor2VerifyJusticeArgs {
            file: a.file,
            rung: a.rung,
            timeout_ms: a.timeout_ms,
            parallel_rungs: a.parallel_rungs,
            json: a.json,
            ci: a.ci,
        });
    }
    if !a.responses.is_empty() {
        return btor2_verify_liveness_all(Btor2VerifyLivenessAllArgs {
            file: a.file,
            responses: a.responses,
            ci: a.ci,
        });
    }
    let (Some(request), Some(grant)) = (a.request, a.grant) else {
        return Err(
            "verify-liveness: give --request and --grant, one or more --response, or --justice"
                .to_string(),
        );
    };
    if !a.fairness.is_empty() {
        return btor2_verify_liveness_under_fairness(Btor2VerifyLivenessUnderFairnessArgs {
            file: a.file,
            request,
            grant,
            fairness: a.fairness,
            ci: a.ci,
        });
    }
    btor2_verify_liveness(Btor2LivenessOneArgs {
        file: a.file,
        request,
        grant,
        ci: a.ci,
    })
}

fn btor2_verify_liveness(args: Btor2LivenessOneArgs) -> Result<(), String> {
    use mununu_core::adapter::liveness_rescue::{
        parse_response_atom, response_liveness_rescue_atoms,
    };
    use mununu_core::verdict::PropertyVerdict;

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let request = parse_response_atom(&args.request)?;
    let grant = parse_response_atom(&args.grant)?;

    let (verdict, outcome) = response_liveness_rescue_atoms(&content, &request, &grant, false)
        .ok_or_else(|| {
            format!(
                "could not build the liveness monitor for '{}' — an atom likely binds no signal",
                args.file.display()
            )
        })?;

    let summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "property": format!("AG(({}) -> AF ({}))", args.request, args.grant),
        "outcome": PropertyVerdict::from(verdict).as_str(),
        "decided_by": outcome.reachable_by.iter().chain(outcome.unreachable_by.iter())
            .collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    ci_gate_exit(PropertyVerdict::from(verdict).as_str(), args.ci.fail_on);
    Ok(())
}

/// `mununu btor2 verify-liveness --response` — decide the conjunction of response-liveness
/// properties `⋀ᵢ AG(aᵢ → AF bᵢ)` from repeatable `--response "ANTE => CONS"` pairs,
/// via the l2s reduction + the portfolio, printing the combined verdict + per-response
/// `decided_by` as JSON. Surface peer of `POST /api/v1/btor2/verify-liveness`.
fn btor2_verify_liveness_all(args: Btor2VerifyLivenessAllArgs) -> Result<(), String> {
    use mununu_core::adapter::liveness_rescue::{
        parse_response_pairs, response_conjunction_property, response_liveness_rescue_conjunction,
    };
    use mununu_core::verdict::PropertyVerdict;

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let pairs = parse_response_pairs(&args.responses)?;

    let (verdict, outcomes) = response_liveness_rescue_conjunction(&content, &pairs, false)
        .ok_or_else(|| {
            format!(
                "could not build the liveness monitor for '{}' — an atom likely binds no signal",
                args.file.display()
            )
        })?;

    let summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "property": response_conjunction_property(&args.responses),
        "outcome": PropertyVerdict::from(verdict).as_str(),
        "responses": per_response_decided_by(&args.responses, &outcomes),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    ci_gate_exit(PropertyVerdict::from(verdict).as_str(), args.ci.fail_on);
    Ok(())
}

/// `mununu btor2 verify-liveness --fairness` (mununu#477 Option B) — decide
/// `(⋀ GF fair) → AG(request → AF grant)` via the fair-cycle l2s (Emerson–Lei)
/// plus the reachability portfolio, printing the verdict as JSON. Surface peer
/// of `POST /api/v1/btor2/verify-liveness`.
fn btor2_verify_liveness_under_fairness(
    args: Btor2VerifyLivenessUnderFairnessArgs,
) -> Result<(), String> {
    use mununu_core::adapter::liveness_rescue::{
        parse_fairness_atoms, parse_response_atom, response_liveness_rescue_under_fairness,
    };
    use mununu_core::verdict::PropertyVerdict;

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let request = parse_response_atom(&args.request)?;
    let grant = parse_response_atom(&args.grant)?;
    let fairness = parse_fairness_atoms(&args.fairness)?;

    let (verdict, outcome) =
        response_liveness_rescue_under_fairness(&content, &request, &grant, &fairness, false)
            .ok_or_else(|| {
                format!(
                    "could not build the fair-cycle liveness monitor for '{}' — an atom likely \
                     binds no signal",
                    args.file.display()
                )
            })?;

    let property = if args.fairness.is_empty() {
        format!("AG(({}) -> AF ({}))", args.request, args.grant)
    } else {
        let fair_and = args
            .fairness
            .iter()
            .map(|f| format!("GF ({f})"))
            .collect::<Vec<_>>()
            .join(" && ");
        format!(
            "({fair_and}) -> AG(({}) -> AF ({}))",
            args.request, args.grant
        )
    };
    let summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "property": property,
        "outcome": PropertyVerdict::from(verdict).as_str(),
        "fairness": args.fairness,
        "decided_by": outcome.reachable_by.iter().chain(outcome.unreachable_by.iter())
            .collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    ci_gate_exit(PropertyVerdict::from(verdict).as_str(), args.ci.fail_on);
    Ok(())
}

/// Build the per-response `[{ response, decided_by }]` JSON for the
/// `verify-liveness --response` summaries (BTOR2- and SV-direct), pairing each `"ANTE => CONS"`
/// input with the engines that decided its `bad`-reachability query.
fn per_response_decided_by(
    responses: &[String],
    outcomes: &[mununu_core::adapter::reach_portfolio::ReachOutcome],
) -> Vec<mununu_core::api::models::SvLivenessResponseView> {
    responses
        .iter()
        .zip(outcomes.iter())
        .map(|(r, o)| mununu_core::api::models::SvLivenessResponseView {
            response: r.trim().to_string(),
            decided_by: o
                .reachable_by
                .iter()
                .chain(o.unreachable_by.iter())
                .map(|e| e.to_string())
                .collect(),
        })
        .collect()
}

/// `mununu btor2 verify` — decide `bad`-reachability with the multi-engine safety
/// portfolio and print the canonical property verdict (`holds` = `bad` unreachable,
/// `violated` = reachable, `unknown` = undecided) + the per-engine reachability
/// breakdown as JSON. Surface peer of the API `POST /api/v1/btor2/verify`; both share
/// the verdict label via [`mununu_core::verdict::PropertyVerdict`].
/// Internal `btor2 spacer-check`: read a BTOR2 design from stdin, run z3-SPACER, print
/// `safe` / `unsafe` / `unknown`. The reachability portfolio self-execs this so SPACER
/// runs in a throwaway child — z3's Fixedpoint can flaky-segfault, and isolating it
/// keeps a crash from taking down the whole verify. Always exits 0 on a clean run; a
/// segfault in the child is what the parent reads as "abstain".
fn btor2_spacer_check(args: Btor2SpacerCheckArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::native_bmc::SafetyVerdict;
    use std::io::Read;
    let mut content = String::new();
    std::io::stdin()
        .read_to_string(&mut content)
        .map_err(|e| format!("btor2 spacer-check: read stdin: {e}"))?;
    // A parse failure abstains (`unknown`) rather than erroring — the parent treats any
    // non-verdict output as abstain, so a hard error would just be noise on this path.
    let verdict = match mununu_core::adapter::btor2::parser::parse(&content) {
        Ok(file) => match mununu_core::adapter::btor2::native_spacer::decide_bad_safety_spacer(
            &file,
            Some(args.timeout_ms),
        ) {
            Ok(SafetyVerdict::Safe { .. }) => "safe",
            Ok(SafetyVerdict::Violated { .. }) => "unsafe",
            _ => "unknown",
        },
        Err(_) => "unknown",
    };
    println!("{verdict}");
    Ok(())
}

/// `mununu btor2 inject-fault FILE --fault REG[:k]… [-o PATH]`.
fn btor2_inject_fault(args: Btor2InjectFaultArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::fault::{FaultSpec, inject_faults};
    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    let file = mununu_core::adapter::btor2::parser::parse(&content)
        .map_err(|e| format!("BTOR2 parse error in '{}': {e}", args.file.display()))?;
    let spec = FaultSpec::parse_many(&args.fault, args.fault_warmup)?;
    let (out, injected) = inject_faults(&file, &spec).map_err(|m| format!("inject-fault: {m}"))?;
    let mut text = format!("; mununu btor2 inject-fault — {}\n", spec.as_label());
    for i in &injected {
        text.push_str(&format!(
            "; site {} ({} bit(s)): enable `{}`, value `{}`\n",
            i.register, i.width, i.en_input, i.val_input
        ));
    }
    text.push_str(&mununu_core::adapter::btor2::emit::emit_btor2(&out));
    match &args.output {
        Some(path) => std::fs::write(path, &text)
            .map_err(|e| format!("Failed to write '{}': {e}", path.display())),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

fn btor2_verify(args: Btor2VerifyArgs) -> Result<(), String> {
    use mununu_core::adapter::reach_portfolio::{
        decide_reach_owned_only, decide_reach_portfolio,
        decide_reach_portfolio_parallel_with_timeout,
    };
    use mununu_core::verdict::PropertyVerdict;

    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;
    // `--engine cube|ic3`: one safety engine instead of the portfolio (formerly `verify-safety`).
    if !matches!(args.engine, SafetyEngineArg::Portfolio) {
        use mununu_core::adapter::recoverability::{SafetyEngine, verify_safety_with_engine};
        if !args.fault.is_empty() || args.target.is_some() || args.witness || args.owned_only {
            return Err(
                "--engine cube|ic3 takes none of --fault, --target, --witness, \
                        --owned-only (they are portfolio options)"
                    .to_string(),
            );
        }
        let engine = match args.engine {
            SafetyEngineArg::Ic3 => SafetyEngine::Ic3,
            _ => SafetyEngine::Cube,
        };
        let (verdict, detail) = verify_safety_with_engine(&content, engine)?;
        let decided = |v: PropertyVerdict| {
            if verdict == v {
                vec![engine.as_str()]
            } else {
                Vec::new()
            }
        };
        let summary = serde_json::json!({
            "file": args.file.display().to_string(),
            "engines": engine.as_str(),
            "outcome": verdict.as_str(),
            "reachable_by": decided(PropertyVerdict::Violated),
            "unreachable_by": decided(PropertyVerdict::Holds),
            "contradiction": false,
            "detail": detail,
        });
        print_json_summary(&summary)?;
        ci_gate_exit(verdict.as_str(), args.ci.fail_on);
        return Ok(());
    }
    // `--fault … --target ATOM`: the k-fault adversary (`fault_adversary::verify_fault_reach`).
    if let Some(target) = &args.target {
        use mununu_core::adapter::btor2::fault::FaultSpec;
        let spec = FaultSpec::parse_many(&args.fault, args.fault_warmup)?;
        let o = mununu_core::adapter::fault_adversary::verify_fault_reach(
            &content,
            &spec,
            Some(target),
            args.witness_max_k,
        )?;
        let summary = serde_json::json!({
            "file": args.file.display().to_string(),
            "engines": "fault-adversary(reach portfolio)",
            "fault": o.fault,
            "target": o.target,
            "outcome": o.verdict,
            "decided_by": o.decided_by,
            "trace": (o.verdict == "violated")
                .then(|| mununu_core::api::models::Trace::attack(o.depth, &o.attack)),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .map_err(|e| format!("serialize summary: {e}"))?
        );
        ci_gate_exit(o.verdict, args.ci.fail_on);
        return Ok(());
    }
    let file = mununu_core::adapter::btor2::parser::parse(&content)
        .map_err(|e| format!("BTOR2 parse error in '{}': {e}", args.file.display()))?;
    // `--fault`: the fault-injection rewrite before any engine (what `btor2 inject-fault`
    // prints); the summary names the posture so a `violated` reads as an attack.
    let (file, fault_label) = if args.fault.is_empty() {
        (file, None)
    } else {
        use mununu_core::adapter::btor2::fault::{FaultSpec, inject_faults};
        let spec = FaultSpec::parse_many(&args.fault, args.fault_warmup)?;
        let (out, _) = inject_faults(&file, &spec).map_err(|m| format!("--fault: {m}"))?;
        (out, Some(spec.as_label()))
    };

    // `--bmc-only`: bounded reachability, no portfolio / no proof. A shallow `bad` ⇒ a sound
    // `violated`; no `bad` within the bound ⇒ a BOUNDED `unknown` (not a safety proof). The
    // bounded-miter path for wide-datapath equivalence checks whose full proof is intractable.
    if args.bmc_only {
        use mununu_core::adapter::btor2::native_bmc::{self, BmcOutcome};
        let (verdict, depth) = match native_bmc::bmc_bad_reachable(&file, args.bmc_k) {
            Ok(BmcOutcome::Violated { depth }) => ("violated", Some(depth)),
            Ok(BmcOutcome::NoCexWithin { .. }) => ("unknown", None),
            Err(e) => return Err(format!("bmc-only: {e:?}")),
        };
        let summary = serde_json::json!({
            "file": args.file.display().to_string(),
            "engines": format!("bmc-only(k={})", args.bmc_k),
            "fault": fault_label,
            "outcome": verdict,
            "reachable_by": if verdict == "violated" { vec!["native-bmc"] } else { Vec::<&str>::new() },
            "unreachable_by": Vec::<&str>::new(),
            "contradiction": false,
            "bmc_depth": depth,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .map_err(|e| format!("serialize summary: {e}"))?
        );
        ci_gate_exit(verdict, args.ci.fail_on);
        return Ok(());
    }

    // `--owned-only`: mununu-owned engines only (no external SPACER/btormc/Pono).
    // Otherwise the full parallel portfolio — identical merge to the sequential
    // driver, but wall-clock bounded by the slowest single engine. `--timeout-ms`
    // raises the subprocess (btormc/Pono) budget so their incremental SAT reaches
    // deeper counterexamples.
    let outcome = if args.owned_only {
        let ms = args.timeout_ms.unwrap_or(60_000);
        decide_reach_owned_only(&file, u32::try_from(ms).unwrap_or(u32::MAX))
    } else if let Some(ms) = args.timeout_ms {
        decide_reach_portfolio_parallel_with_timeout(&file, std::time::Duration::from_millis(ms))
    } else {
        decide_reach_portfolio(&file)
    };

    // `--witness`: on a `violated` (Reachable) verdict, re-derive the shallowest
    // concrete init→bad trace with the bit-precise native BMC engine. The portfolio
    // itself only reports WHICH engines proved reachability; the actionable payload —
    // the exact per-cycle stimulus that trips the assertion — is what an LLM
    // refinement loop consumes. Bounded to `--witness-max-k`; if the bound doesn't
    // reach the `bad` node (a very deep CEX another engine found), the witness is
    // simply omitted (null) rather than fabricated.
    let (trace, trace_unavailable_within_k) = if args.witness
        && outcome.verdict == mununu_core::adapter::reach_portfolio::ReachVerdict::Reachable
    {
        use mununu_core::adapter::btor2::native_bmc::{BmcOutcome, bmc_bad_reachable_witness};
        match bmc_bad_reachable_witness(&file, args.witness_max_k) {
            Ok((BmcOutcome::Violated { .. }, Some(t))) => {
                (Some(mununu_core::api::models::Trace::from_bmc(&t)), None)
            }
            // Reachable per the portfolio but the bounded native re-derivation didn't
            // reach it within `--witness-max-k` — report the bound honestly.
            _ => (None, Some(args.witness_max_k)),
        }
    } else {
        (None, None)
    };

    let summary = serde_json::json!({
        "file": args.file.display().to_string(),
        "engines": if args.owned_only { "owned-only" } else { "full-portfolio" },
        "fault": fault_label,
        "outcome": PropertyVerdict::from(outcome.verdict).as_str(),
        "reachable_by": outcome.reachable_by,
        "unreachable_by": outcome.unreachable_by,
        "contradiction": outcome.verdict
            == mununu_core::adapter::reach_portfolio::ReachVerdict::Contradiction,
        "trace": trace,
        "trace_unavailable_within_k": trace_unavailable_within_k,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    ci_gate_exit(
        PropertyVerdict::from(outcome.verdict).as_str(),
        args.ci.fail_on,
    );
    Ok(())
}

/// R.5 Item 3 sub-item 3.5 (2026-06-04) — CEGAR refinement
/// loop CLI handler.
///
/// R-S8 session 2 (2026-06-08) — parse the `--init-values
/// REG=v1,v2,v3` CLI flag(s) and build an `AdapterOptions` whose
/// `sidecar_json` declares one `signals[]` entry per flag entry
/// with the listed `config_values`. The CEGAR loop reads this via
/// `r_s8_encoder::sidecar_config_values` and threads it through
/// the predicate-cube lift's `config_values`.
///
/// Returns an `AdapterOptions::default()` when the flag is absent.
/// Errors on malformed entries (missing `=`, non-numeric values).
fn build_adapter_options_with_config_values(
    config_values_args: &[String],
) -> Result<mununu_core::adapter::AdapterOptions, String> {
    use mununu_core::adapter::AdapterOptions;
    // M.6 parity (2026-06-20) — the `REG=v1,v2,...` parse moved to the shared
    // core helper `adapter::btor2::cegar::config_values_to_sidecar_json` so the
    // CLI `--init-values` and the API `config_values` field parse identically
    // (single source of truth; the format can't drift between surfaces).
    let sidecar_json =
        mununu_core::adapter::btor2::cegar::config_values_to_sidecar_json(config_values_args)?;
    Ok(AdapterOptions {
        sidecar_json,
        ..AdapterOptions::default()
    })
}

/// Parses the user-supplied formula + initial predicate set,
/// builds a [`CegarOptions`] with the selected predicate source,
/// and invokes [`cegar_refine_loop`] on the BTOR2 fixture.
/// Prints a human-readable or JSON summary of the resulting
/// [`CegarTrace`].
fn btor2_cegar(args: Btor2CegarArgs) -> Result<(), String> {
    if !args.file.exists() {
        return Err(format!(
            "BTOR2 input file does not exist: {}",
            args.file.display()
        ));
    }
    let content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read BTOR2 '{}': {e}", args.file.display()))?;

    run_cegar_cli(
        &content,
        &args.file.display().to_string(),
        CegarCliParams {
            formula: &args.formula,
            predicates: &args.predicates,
            predicate_source: args.predicate_source,
            cvc5_path: args.cvc5_path.as_deref(),
            max_iterations: args.max_iterations,
            must_edge_inference: args.must_edge_inference,
            may_edge_inference: args.may_edge_inference,
            config_values: &args.init_values,
            controllable_inputs: &args.controllable_inputs,
            sv_source: args.sv_source.as_deref(),
            sidecar: args.sidecar.as_deref(),
            emit_ctxdsl: args.emit_ctxdsl.as_deref(),
            json: args.json,
            engine: args.engine,
        },
    )
}

/// cegar-extraction Stage 2 (2026-06-22) — SV-direct CEGAR in one
/// command: lift SystemVerilog to a single flattened BTOR2 (sv2v +
/// Yosys) and run the same predicate-abstraction refinement loop as
/// `btor2 cegar`. Surface peer of the API `POST /api/v1/sv/cegar`.
fn sv_cegar(args: SvCegarArgs) -> Result<(), String> {
    use std::collections::HashMap;

    let primary_content = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read SV source '{}': {e}", args.file.display()))?;
    let mut additional: HashMap<String, String> = HashMap::new();
    for src in &args.sources {
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Invalid additional source path: {}", src.display()))?;
        let body = std::fs::read_to_string(src)
            .map_err(|e| format!("Failed to read additional source '{}': {e}", src.display()))?;
        additional.insert(name.to_string(), body);
    }

    let yopts = mununu_core::adapter::yosys::YosysOptions {
        top: args.top.clone(),
        additional_sources: additional.into_iter().collect(),
        primary_source_path: Some(args.file.display().to_string()),
        use_sv2v: args.frontend.sv2v(),
        frontend: args.frontend.into(),
        setundef_anyseq: args.setundef_anyseq,
        setundef_anyconst: args.setundef_anyconst,
        ..Default::default()
    };
    // SV → single flattened BTOR2 (the predicate-cube lift wants one
    // transition system, not the per-module split).
    let btor2 = mununu_core::adapter::yosys::sv_to_btor2(&primary_content, &yopts)
        .map_err(|e| format!("sv cegar: SV → BTOR2 (sv2v + Yosys): {}", e.message))?;

    run_cegar_cli(
        &btor2,
        &args.file.display().to_string(),
        CegarCliParams {
            formula: &args.formula,
            predicates: &args.predicates,
            predicate_source: args.predicate_source,
            cvc5_path: args.cvc5_path.as_deref(),
            max_iterations: args.max_iterations,
            must_edge_inference: args.must_edge_inference,
            may_edge_inference: args.may_edge_inference,
            config_values: &args.init_values,
            controllable_inputs: &args.controllable_inputs,
            // The SV input itself is the reset-simulation source.
            sv_source: Some(args.file.as_path()),
            sidecar: args.sidecar.as_deref(),
            emit_ctxdsl: args.emit_ctxdsl.as_deref(),
            json: args.json,
            engine: args.engine,
        },
    )
}

/// XL.6a — `mununu sv extract-sva`: run the slang SVA front-end over an SV
/// source set and print the translated mu-calculus property set.
fn sv_extract_sva(args: SvExtractSvaArgs) -> Result<(), String> {
    let primary_name = args
        .file
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("Invalid SV source path: {}", args.file.display()))?;
    let primary = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read SV source '{}': {e}", args.file.display()))?;
    let mut sources: Vec<(String, String)> = vec![(primary_name.to_string(), primary)];
    for src in &args.sources {
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Invalid additional source path: {}", src.display()))?;
        let body = std::fs::read_to_string(src)
            .map_err(|e| format!("Failed to read additional source '{}': {e}", src.display()))?;
        sources.push((name.to_string(), body));
    }

    let report = mununu_core::adapter::slang::extract::extract_sva(&sources)
        .map_err(|e| format!("sv extract-sva: {}", e.message))?;

    if args.json {
        render_extract_sva_json(&report);
    } else {
        render_extract_sva_text(&report);
    }
    Ok(())
}

fn sva_kind_str(kind: mununu_core::adapter::slang::translate::SvaKind) -> &'static str {
    use mununu_core::adapter::slang::translate::SvaKind;
    match kind {
        SvaKind::Assert => "assert",
        SvaKind::Assume => "assume",
        SvaKind::Cover => "cover",
    }
}

fn render_extract_sva_text(report: &mununu_core::adapter::slang::translate::TranslationReport) {
    println!(
        "SVA extraction: {} translated, {} unsupported (of {} concurrent assertions)",
        report.translated.len(),
        report.unsupported.len(),
        report.total()
    );
    for t in &report.translated {
        println!("  [{}] {}: {}", sva_kind_str(t.kind), t.name, t.formula);
        if let Some(c) = &t.recoverability_companion {
            println!("        recoverability (AG EF): {c}");
        }
    }
    for u in &report.unsupported {
        let kind = u.kind.map(sva_kind_str).unwrap_or("?");
        println!("  [unsupported {}] {}: {}", kind, u.name, u.reason);
    }
    if !report.required_shadows.is_empty() {
        let shadows: Vec<String> = report
            .required_shadows
            .iter()
            .map(|s| {
                if s.depth > 1 {
                    format!("{}({}) x{} deep", s.base, s.width, s.depth)
                } else {
                    format!("{}({})", s.base, s.width)
                }
            })
            .collect();
        println!("  required __past shadow registers: {}", shadows.join(", "));
    }
}

fn render_extract_sva_json(report: &mununu_core::adapter::slang::translate::TranslationReport) {
    let translated: Vec<serde_json::Value> = report
        .translated
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "kind": sva_kind_str(t.kind),
                "formula": t.formula,
                "recoverability_companion": t.recoverability_companion,
            })
        })
        .collect();
    let unsupported: Vec<serde_json::Value> = report
        .unsupported
        .iter()
        .map(|u| {
            serde_json::json!({
                "name": u.name,
                "kind": u.kind.map(sva_kind_str),
                "reason": u.reason,
            })
        })
        .collect();
    let required_shadows: Vec<serde_json::Value> = report
        .required_shadows
        .iter()
        .map(|s| serde_json::json!({ "base": s.base, "width": s.width, "depth": s.depth }))
        .collect();
    let out = serde_json::json!({
        "translated": translated,
        "unsupported": unsupported,
        "required_shadows": required_shadows,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string())
    );
}

/// XL.6b — `mununu sv verify-auto`: extract SVA, lift, and verify each property
/// against the model with no sidecar.
/// `--pin SIGNAL=VALUE` entries → the pinned inputs (shared by `sv verify-auto` and
/// `sv mutate`, mununu#725). H.J.b / mununu#459: a malformed entry is a HARD error, never a
/// silent drop; a value that is not a decimal u64 (e.g. hex `0x1`) is rejected with a message
/// naming the value. An unknown / non-input SIGNAL is caught downstream in `verify_auto`
/// against the model's real primary inputs.
fn parse_config_values(
    entries: &[String],
) -> Result<std::collections::HashMap<String, u64>, String> {
    entries
        .iter()
        .map(|e| {
            let (name, val) = e
                .split_once('=')
                .ok_or_else(|| format!("malformed --pin '{e}': expected SIGNAL=VALUE"))?;
            let (name, val) = (name.trim(), val.trim());
            if name.is_empty() {
                return Err(format!("malformed --pin '{e}': SIGNAL must be non-empty"));
            }
            let value = val.parse::<u64>().map_err(|_| {
                format!(
                    "malformed --pin '{e}': VALUE must be a decimal u64 \
                     (got '{val}'); hex/0x, signed, and non-numeric values are not accepted"
                )
            })?;
            Ok((name.to_string(), value))
        })
        .collect()
}

/// `--lift-override NAME=PATH` entries → `(name, file content)` pairs (shared by `sv verify-auto`
/// and `sv mutate`, mununu#725). A malformed entry or an unreadable PATH is a hard error; a NAME
/// matching no source is refused inside `verify_auto`.
fn read_lift_overrides(entries: &[String]) -> Result<Vec<(String, String)>, String> {
    entries
        .iter()
        .map(|e| {
            let (name, path) = e
                .split_once('=')
                .filter(|(n, p)| !n.trim().is_empty() && !p.trim().is_empty())
                .ok_or_else(|| format!("malformed --lift-override '{e}': expected NAME=PATH"))?;
            let body = std::fs::read_to_string(path.trim())
                .map_err(|err| format!("Failed to read --lift-override '{e}': {err}"))?;
            Ok((name.trim().to_string(), body))
        })
        .collect()
}

fn sv_verify_auto(args: SvVerifyAutoArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::kmts_lift::MustEdgeInference;
    use mununu_core::adapter::slang::verify_auto::{PortfolioMode, VerifyAutoOptions, verify_auto};

    let lift = read_sv_lift(&args.lift)?;
    let primary_name = lift.primary_name.clone();
    let mut sources: Vec<(String, String)> = vec![(primary_name.clone(), lift.source.clone())];
    sources.extend(lift.additional_sources.iter().cloned());
    // The overrides are verify-auto's own option (they replace sources for the lift only, while
    // the SVA is extracted from the originals), so the yosys options carry the plain sources.
    let yopts = mununu_core::adapter::yosys::YosysOptions {
        primary_source_path: Some(args.lift.primary_display()),
        ..lift.yosys_options()
    };
    // mununu#663 — `auto` (the default) resolves the policy per property; an explicit `off`
    // switches that resolution off and keeps the cube may-only.
    let must_edge_auto = matches!(args.must_edge_inference, MustEdgeInferenceArg::Auto);
    let must_edge_inference = match args.must_edge_inference {
        MustEdgeInferenceArg::Auto | MustEdgeInferenceArg::Off => MustEdgeInference::Off,
        MustEdgeInferenceArg::SmtPerTarget => MustEdgeInference::SmtPerTarget,
        MustEdgeInferenceArg::SmtPerTargetStandard => MustEdgeInference::SmtPerTargetStandard,
        MustEdgeInferenceArg::SmtHyperMust => MustEdgeInference::SmtHyperMust,
    };
    let config_values = parse_config_values(&args.pin)?;
    // H.H — parse `SIGNAL<=VALUE` (or `SIGNAL=VALUE`) counter-bound entries; both
    // spellings mean the inclusive upper bound `SIGNAL <= VALUE`.
    let counter_bounds: std::collections::HashMap<String, u64> = args
        .counter_bound
        .iter()
        .filter_map(|e| {
            let (name, val) = e.split_once("<=").or_else(|| e.split_once('='))?;
            Some((name.trim().to_string(), val.trim().parse::<u64>().ok()?))
        })
        .collect();
    let lift_overrides = lift.lift_overrides.clone();
    let opts = VerifyAutoOptions {
        max_iterations: args.max_iterations,
        must_edge_inference,
        must_edge_auto,
        gate_reset: !args.no_gate_reset,
        auto_stub_flops: !args.no_auto_stub_flops,
        config_values,
        counter_bounds,
        predicate_hints: args.predicate.clone(),
        templates: template_refs(&args.template, &args.template_args)?,
        templates_files: args.templates_files.clone(),
        // R-F5.5d — `--engine symbolic` routes every property through the R-F5
        // BDD CEGAR loop (no per-cube-pair SMT).
        symbolic_engine: args.engine == EngineArg::Symbolic,
        // D1.6 — `--engine exact-symbolic` decides each property exactly over the
        // full bit-blasted state (definite verdict, never ⊥).
        exact_symbolic: args.engine == EngineArg::ExactSymbolic,
        // PORTFOLIO — `--engine portfolio-sequential|-parallel` runs several engines
        // and merges (ignores the two single-engine flags above).
        portfolio: match args.engine {
            EngineArg::PortfolioSequential => Some(PortfolioMode::Sequential),
            EngineArg::PortfolioParallel => Some(PortfolioMode::Parallel),
            _ => None,
        },
        rescue_bottom_safety: !args.no_rescue,
        rescue_bottom_liveness: !args.no_rescue,
        rescue_bottom_recoverability: !args.no_rescue,
        // `sv verify-auto` verifies the design as-lifted; mutation is the `sv mutate` verb's job.
        mutation: None,
        faults: mununu_core::adapter::btor2::fault::FaultPosture::parse(
            &args.fault,
            args.fault_model.as_deref(),
            args.fault_warmup,
        )?,
        // mununu#677 — read by nothing; the shadow synthesis is gone. `--no-input-state` is
        // the refusal switch.
        antecedent_shadow: true,
        // mununu#629 — --no-antecedent-propagate leaves the formula as lifted.
        antecedent_propagate: !args.no_antecedent_propagate,
        // mununu#656 — --no-input-state restores the input refusal.
        input_state: !args.no_input_state,
        lift_overrides,
        // PR C — planner-only fields: the CLI run re-plans its own rows and truncates
        replan_defer: false,
        breadcrumb_append: false,
        skip_rows: std::collections::HashSet::new(),
        assume_mode: match args.assumes {
            AssumesArg::Precondition => {
                mununu_core::adapter::slang::verify_auto::AssumeMode::Precondition
            }
            AssumesArg::Check => mununu_core::adapter::slang::verify_auto::AssumeMode::Check,
            AssumesArg::Ignore => mununu_core::adapter::slang::verify_auto::AssumeMode::Ignore,
        },
    };

    let report = verify_auto(&sources, &yopts, &opts)
        .map_err(|e| format!("sv verify-auto: {}", e.message))?;

    // mununu#537 — evaluate the caller's claims against the report. `None` when nothing was
    // claimed, which leaves the ordinary verdict gate in charge.
    let expectations = args.expect.to_expectations()?;
    let expectation_result = if expectations.is_empty() {
        None
    } else {
        Some(mununu_core::adapter::slang::expectations::evaluate(
            &report,
            &expectations,
        ))
    };
    if let Some(dir) = &args.emit_testbench {
        emit_replay_testbenches(dir, &args, &sources, &report)?;
    }
    if args.json {
        render_verify_auto_json(&report, &expectation_result)?;
    } else {
        render_verify_auto_text(&report);
        render_expectations_text(&expectation_result);
    }
    // mununu#537 — when the caller DECLARED expectations, they supersede the verdict gate.
    //
    // This is required, not cosmetic: under `--expect-violated` the ordinary gate would exit 2 on
    // the very violation that was asked for, so a satisfied contract could never exit 0. Exit 4
    // says "a verdict was not what you claimed", distinct from 1 ("the run failed") and from the
    // 2/3 the verdict gate uses when no claim was made.
    if let Some(result) = &expectation_result {
        if !result.satisfied {
            std::process::exit(EXPECTATIONS_UNMET_EXIT);
        }
        return Ok(());
    }
    // CI gate: the most severe property verdict drives the exit code.
    let worst = worst_verdict(
        report
            .properties
            .iter()
            .map(|p| verify_outcome_canonical(&p.outcome)),
    );
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

/// mununu#537 — the human-readable expectation verdict, printed after the report.
fn render_expectations_text(
    result: &Option<mununu_core::adapter::slang::expectations::ExpectationResult>,
) {
    let Some(result) = result else { return };
    if result.satisfied {
        println!("\nExpectations: MET");
        return;
    }
    println!(
        "\nExpectations: NOT MET ({} failure(s))",
        result.failures.len()
    );
    for f in &result.failures {
        match &f.property {
            Some(name) => println!("  [expect] {name}: {}", f.reason),
            None => println!("  [expect] {}", f.reason),
        }
    }
}

/// The canonical `PropertyVerdict` label for a `verify-auto` outcome.
fn verify_outcome_canonical(
    outcome: &mununu_core::adapter::slang::verify_auto::VerifyOutcome,
) -> &'static str {
    use mununu_core::adapter::slang::verify_auto::VerifyOutcome;
    match outcome {
        VerifyOutcome::Holds => "holds",
        VerifyOutcome::Violated { .. } => "violated",
        VerifyOutcome::Unknown { .. } => "unknown",
        VerifyOutcome::Skipped { .. } => "skipped",
    }
}

fn verify_outcome_str(outcome: &mununu_core::adapter::slang::verify_auto::VerifyOutcome) -> String {
    use mununu_core::adapter::slang::verify_auto::VerifyOutcome;
    match outcome {
        VerifyOutcome::Holds => "HOLDS".to_string(),
        VerifyOutcome::Violated { false_cells } => format!("VIOLATED ({false_cells} cell(s))"),
        VerifyOutcome::Unknown { unknown_cells } => {
            format!("UNKNOWN/\u{22a5} ({unknown_cells} cell(s))")
        }
        VerifyOutcome::Skipped { reason } => format!("skipped — {reason}"),
    }
}

fn render_verify_auto_text(report: &mununu_core::adapter::slang::verify_auto::AutoVerifyReport) {
    println!(
        "verify-auto: {} propert{} verified, {} unsupported assertion(s)",
        report.properties.len(),
        if report.properties.len() == 1 {
            "y"
        } else {
            "ies"
        },
        report.unsupported.len()
    );
    println!(
        "  model: {} state register(s)",
        report.diagnostics.state_register_count
    );
    if !report.diagnostics.blackboxed_modules.is_empty() {
        println!(
            "  black-boxed (cut to free inputs — provide source to model): {}",
            report.diagnostics.blackboxed_modules.join(", ")
        );
    }
    if !report.diagnostics.auto_provided_stubs.is_empty() {
        println!(
            "  auto-stubbed flop primitives (behavioral model injected): {}",
            report.diagnostics.auto_provided_stubs.join(", ")
        );
    }
    if !report.diagnostics.gated_resets.is_empty() {
        println!(
            "  reset-gated (pinned inactive): {}",
            report.diagnostics.gated_resets.join(", ")
        );
    }
    for p in &report.properties {
        println!(
            "  [{}] {}: {}",
            sva_kind_str(p.kind),
            p.name,
            verify_outcome_str(&p.outcome)
        );
        if let Some(label) = p.label.as_deref().filter(|l| *l != p.name) {
            println!("        label: {label}");
        }
        println!("        formula: {}", p.formula);
        // monono ask 26 — WHY this property is ⊥, on the property's own lines rather than in a
        // note. A consumer that filters notes (monono's formal lane did, wholesale) still sees
        // this, and for a resource abstain the text is the ENGINE'S OWN message, which names the
        // budget it hit and its numbers. Before this, a ⊥ arrived as a bare outcome plus a cube
        // count and the only available move was to try budget knobs blind.
        if let Some(r) = &p.bottom_reason {
            // mununu#553 ask 1+2 — the tag says WHAT stopped, the budget says WHICH knob, and the
            // determinism says whether re-running could give a different answer. A host-dependent
            // ⊥ and a reproducible one call for opposite responses and used to render identically.
            let budget = r
                .engine_budget()
                .map(|b| match b.knob() {
                    Some(k) => format!(" budget={} ({k})", b.tag()),
                    None => format!(" budget={}", b.tag()),
                })
                .unwrap_or_default();
            println!(
                "        bottom-reason [{}]{budget} determinism={}: {}",
                r.tag(),
                r.determinism().tag(),
                r.one_line()
            );
        }
        // mununu#599 — what the rest of the report says about a recoverability guarantee's
        // vacuity, on the guarantee's own lines: a HOLDS whose target is invariant, or whose
        // same-register witness is refuted, must not read like any other HOLDS.
        if let Some(v) = &p.vacuity {
            println!(
                "        vacuity [{}] witness={}: {}",
                v.kind.tag(),
                v.witness,
                v.detail
            );
        }
        if !p.seeded_predicates.is_empty() {
            println!("        predicates: {}", p.seeded_predicates.join(", "));
        }
        // The exact engine's counterexample: either the A.4 unreachable-target witness
        // for a bare `EF p` (reachability) — "the design never reaches <p>", the repair
        // signal a safety check never produces — the reset → violation path for a safety
        // failure, or the D1.8b stall-lasso / trap-path (reset → prefix → repeating ¬p cycle)
        // for a `AF`/`AG AF`/`AG EF` failure.
        if let Some(cex) = &p.counterexample {
            if !cex.unreachable_target.is_empty() {
                println!(
                    "        counterexample: target UNREACHABLE from reset - the design never reaches {}",
                    cex.unreachable_target.join(" ∧ ")
                );
            } else if cex.cycle.is_empty() {
                // A safety violation: the finite path from reset to the failing cycle, with the
                // inputs each cycle read (`inputs[i]` belongs to state `i`). A trap certificate
                // (`AG EF` refuted) reads the same way: the path ends in a closed set the target is
                // outside of, so no cycle is drawn.
                if p.decided_by.as_deref()
                    == Some(mununu_core::adapter::slang::verify_auto::TRAP_CERTIFICATE_DECIDED_BY)
                {
                    println!(
                        "        counterexample (reset -> trap; the trap is closed, the target is never reached again):"
                    );
                } else {
                    println!("        counterexample (reset -> violation):");
                }
                let render =
                    |st: &[(String, mununu_core::adapter::btor2::cex_value::CexValue)]| {
                        st.iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                for (i, st) in cex.prefix.iter().enumerate() {
                    println!("          [{i}] {}", render(st));
                    if let Some(inp) = cex.inputs.get(i).filter(|inp| !inp.is_empty()) {
                        println!("              in: {}", render(inp));
                    }
                }
            } else {
                println!("        counterexample (stall lasso):");
                let render_state =
                    |st: &[(String, mununu_core::adapter::btor2::cex_value::CexValue)]| {
                        st.iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                for st in &cex.prefix {
                    println!("          -> {}", render_state(st));
                }
                for (i, st) in cex.cycle.iter().enumerate() {
                    let marker = if i == 0 { "(*)" } else { "   " };
                    println!("          {marker} {}", render_state(st));
                }
                println!("          (cycle repeats forever - the property is avoided)");
            }
        }
    }
    for (name, reason) in &report.unsupported {
        println!("  [unsupported] {name}: {reason}");
    }
    // H.J — provenance notes: the abstraction / scoping decisions that shaped
    // the verdicts (config concretizations, reset-gating, cut modules, posture).
    if !report.notes.is_empty() {
        println!("\nNotes (decisions that shaped these verdicts):");
        for n in &report.notes {
            println!("  {} [{}] {}", note_level_glyph(n.level), n.kind, n.summary);
            if !n.detail.is_empty() {
                println!("        {}", n.detail);
            }
            if !n.items.is_empty() {
                println!("        · {}", n.items.join(", "));
            }
        }
    }
}

/// A glyph for a note's severity — a scope/soundness caveat stands out from an
/// informational note in the CLI.
fn note_level_glyph(level: mununu_core::adapter::slang::verify_auto::NoteLevel) -> &'static str {
    use mununu_core::adapter::slang::verify_auto::NoteLevel;
    match level {
        NoteLevel::Info => "ℹ",
        NoteLevel::ScopeCaveat => "⚠ scope",
        NoteLevel::SoundnessCaveat => "⚠ soundness",
    }
}

fn render_verify_auto_json(
    report: &mununu_core::adapter::slang::verify_auto::AutoVerifyReport,
    expectations: &Option<mununu_core::adapter::slang::expectations::ExpectationResult>,
) -> Result<(), String> {
    // mununu#536 — serialize the SAME type the HTTP API returns, via the one shared conversion
    // (`impl From<&AutoVerifyReport> for SvVerifyAutoResponse`).
    //
    // This used to be an independent `serde_json::json!` literal, which meant two hand-written
    // serializers for one documented shape. They had already drifted — `detail` was an object
    // here and a string on the API, this one dropped `unreachable_target`, and neither carried
    // the front end used — so a consumer reading
    // `docs/api-schemas/sv-verify-auto-response.schema.json` and then running the CLI got a
    // different document. Going through the shared type means the schema drift test now covers
    // this output too, because it is literally the same type.
    let mut view = mununu_core::api::models::SvVerifyAutoResponse::from(report);
    // mununu#537 — carry WHICH expectation failed in the machine-readable output, so a consumer
    // never has to parse the human lines to find out. That is the point of doing #536 first.
    view.expectations = expectations.as_ref().map(Into::into);
    println!(
        "{}",
        serde_json::to_string_pretty(&view).map_err(|e| format!("serialize report: {e}"))?
    );
    Ok(())
}

/// Shared CEGAR parameters for the CLI handlers. Both `btor2 cegar`
/// (BTOR2-direct) and `sv cegar` (SV lifted to BTOR2 first) populate this
/// from their arg structs and call [`run_cegar_cli`], so the two surfaces
/// stay in lockstep on the CEGAR semantics + report shape.
struct CegarCliParams<'a> {
    formula: &'a str,
    predicates: &'a [String],
    predicate_source: PredicateSourceArg,
    cvc5_path: Option<&'a Path>,
    max_iterations: usize,
    must_edge_inference: MustEdgeInferenceArg,
    may_edge_inference: MayEdgeInferenceArg,
    config_values: &'a [String],
    controllable_inputs: &'a [String],
    sv_source: Option<&'a Path>,
    sidecar: Option<&'a Path>,
    emit_ctxdsl: Option<&'a Path>,
    json: bool,
    engine: EngineArg,
}

/// Run the predicate-abstraction refinement loop over a BTOR2 design
/// (`content`) and print the trace + 3-valued verdict. `fixture_label` is
/// the human-facing source identifier echoed in the report (the BTOR2 path
/// Track I.1 — render a slice of CEGAR witness cells as JSON
/// (`[{ "cube_index": i, "valuation": { "<pred>": <bool>, … } }, …]`).
fn cegar_cells_json(
    cells: &[mununu_core::adapter::btor2::cegar::WitnessCell],
) -> serde_json::Value {
    serde_json::Value::Array(
        cells
            .iter()
            .map(|c| {
                let valuation: serde_json::Map<String, serde_json::Value> = c
                    .valuation
                    .iter()
                    .map(|(name, holds)| (name.clone(), serde_json::Value::Bool(*holds)))
                    .collect();
                serde_json::json!({ "cube_index": c.cube_index, "valuation": valuation })
            })
            .collect(),
    )
}

/// R-F5.4.2b — the `--engine symbolic` path: evaluate the property over the
/// predicate-cube abstraction via the R-F5 symbolic BDD relation (no
/// per-cube-pair SMT), single-shot at the given predicate set. Prints the same
/// `{T, F, ⊥}` verdict-cell tally + outcome shape as the explicit path (minus
/// the refinement-iteration fields, which do not apply).
#[allow(clippy::too_many_arguments)]
fn run_symbolic_cegar_cli(
    content: &str,
    fixture_label: &str,
    predicates: &[mununu_core::adapter::btor2::PredicateSpec],
    formula: &mununu_core::mu_calculus::Formula,
    sidecar: Option<&Path>,
    config_values: &[String],
    max_iterations: usize,
    json: bool,
) -> Result<(), String> {
    use mununu_core::adapter::btor2::symbolic_bitblast::MustSemantics;
    use mununu_core::adapter::btor2::symbolic_engine::{
        SymbolicCegarTermination, symbolic_cegar_refine,
    };

    // The simple `--predicate [NAME:]REG == VALUE` equalities plus any non-derived
    // `compound_predicates` (e.g. `cnt >= 2`) declared in the `--sidecar`. The
    // sidecar overrides the `--init-values` synthetic one (matches the
    // explicit path). Derived/combinational compounds are rejected downstream.
    let mut options = build_adapter_options_with_config_values(config_values)?;
    if let Some(p) = sidecar {
        let sidecar_content = std::fs::read_to_string(p)
            .map_err(|e| format!("Failed to read sidecar '{}': {e}", p.display()))?;
        options.sidecar_json = Some(sidecar_content);
    }
    // R-F5.5b — the symbolic CEGAR loop (WP refinement on ⊥, rebuilding the BDD
    // relation each iteration; no per-cube-pair SMT). `--max-iterations 0` gives
    // the single-shot behaviour.
    let result = symbolic_cegar_refine(
        content,
        predicates,
        &options,
        formula,
        MustSemantics::ForallExists,
        max_iterations,
    )
    .map_err(|e| format!("symbolic cube engine: {}", e.message))?;

    let v = &result.final_verdicts;
    let (t, f, b) = (v.definite_true, v.definite_false, v.bottom);
    let terminated = match result.terminated_with {
        SymbolicCegarTermination::Converged => "converged",
        SymbolicCegarTermination::BoundedIterationsReached => "bounded-iterations-reached",
        SymbolicCegarTermination::PredicateSourceExhausted => "predicate-source-exhausted",
        SymbolicCegarTermination::BudgetExpired => "budget-expired",
    };
    // iteration 0 is the initial evaluation; the rest are refinements.
    let refinements = result.iterations.len().saturating_sub(1);
    let outcome = if b > 0 {
        format!("INDEFINITE — {b} cell(s) need a finer predicate set")
    } else if f > 0 {
        format!("PROPERTY VIOLATED — {f} cell(s) falsify the formula")
    } else {
        format!("PROPERTY HOLDS — all {t} cell(s) satisfy the formula")
    };

    if json {
        let summary = serde_json::json!({
            "fixture": fixture_label,
            "engine": "symbolic",
            "refinement_iterations": refinements,
            "terminated_with": terminated,
            "final_predicate_count": v.num_predicates,
            "feasible_cube_count": v.cube_verdicts.len(),
            "cells": {
                "true_cells": t,
                "false_cells": f,
                "unknown_cells": b,
            },
            "outcome": outcome,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .map_err(|e| format!("serialize summary: {e}"))?
        );
    } else {
        println!("Symbolic predicate-cube CEGAR (R-F5)");
        println!("  fixture:              {fixture_label}");
        println!("  engine:               symbolic (BDD relation, no per-cube-pair SMT)");
        println!("  refinement iterations:{refinements}");
        println!("  terminated_with:      {terminated}");
        println!("  final predicates:     {}", v.num_predicates);
        println!("  feasible cubes:       {}", v.cube_verdicts.len());
        println!("  verdict cells:        T={t} F={f} ⊥={b}");
        println!("  outcome:              {outcome}");
    }
    Ok(())
}

/// for `btor2 cegar`, the SV path for `sv cegar`). Shared by both CLI
/// handlers so identical CEGAR inputs produce identical output.
fn run_cegar_cli(
    content: &str,
    fixture_label: &str,
    params: CegarCliParams<'_>,
) -> Result<(), String> {
    use mununu_core::adapter::btor2::PredicateSpec;
    use mununu_core::adapter::btor2::cegar::{
        CegarOptions, LiftStrategy, PredicateSource, cegar_refine_loop,
    };
    use mununu_core::mu_calculus::{Environment, parser as mu_parser};

    // Honor --cvc5-path via env var (the locate_cvc5 helper
    // reads MUNUNU_CVC5_PATH first). SAFETY: env vars are
    // process-global; this is fine for the CLI handler which
    // is single-threaded + runs once per invocation.
    if let Some(p) = params.cvc5_path {
        unsafe {
            std::env::set_var("MUNUNU_CVC5_PATH", p);
        }
    }

    // Parse initial predicates (`[NAME:]REG == VALUE`, the shared spelling).
    let initial_predicates: Vec<PredicateSpec> = params
        .predicates
        .iter()
        .map(|raw| mununu_core::adapter::recoverability::parse_extra_predicate(raw))
        .collect::<Result<_, _>>()?;
    if initial_predicates.is_empty() {
        return Err(
            "at least one --predicate [NAME:]REG == VALUE is required to bootstrap the cube space"
                .into(),
        );
    }

    // Parse the μ-calculus formula.
    let formula =
        mu_parser::parse(params.formula).map_err(|e| format!("formula parse error: {e:?}"))?;

    // D1.6 — exact symbolic MC is a verify-auto-only engine: it needs the
    // reset-gated model + reset init that `sv verify-auto` builds. The raw
    // `btor2 cegar` / `sv cegar` surfaces are cube-CEGAR (trace output).
    if params.engine == EngineArg::ExactSymbolic {
        return Err(
            "--engine exact-symbolic is available on `sv verify-auto` only (it decides the \
             property exactly over the reset-gated model verify-auto builds). Use `--engine \
             explicit` or `symbolic` here."
                .to_string(),
        );
    }

    // PORTFOLIO is likewise verify-auto-only: it schedules the exact + cube engines over the
    // reset-gated model verify-auto builds. Reject it on the raw cube-CEGAR surfaces.
    if matches!(
        params.engine,
        EngineArg::PortfolioSequential | EngineArg::PortfolioParallel
    ) {
        return Err(
            "--engine portfolio-sequential / portfolio-parallel is available on `sv verify-auto` \
             only (it runs several engines over the reset-gated model verify-auto builds). Use \
             `--engine explicit` or `symbolic` here."
                .to_string(),
        );
    }

    // R-F5.4.2b — the symbolic engine short-circuits the explicit lift + CEGAR
    // loop: it builds the may/must relation as BDDs directly from the BTOR2 and
    // evaluates the formula by BDD image/preimage, avoiding the `O(2^2|P|)` SMT.
    // Single-shot at the given predicate set (no refinement). Simple equality
    // `--predicate`s plus any non-derived `compound_predicates` (e.g. `cnt >= 2`)
    // from the `--sidecar` (R-F5.5a).
    if params.engine == EngineArg::Symbolic {
        return run_symbolic_cegar_cli(
            content,
            fixture_label,
            &initial_predicates,
            &formula,
            params.sidecar,
            params.config_values,
            params.max_iterations,
            params.json,
        );
    }

    // Build the environment with state_count = 2^|predicates|.
    let cube_count = 1usize << initial_predicates.len();
    let env = Environment::new(cube_count);

    // Map the CLI's PredicateSourceArg to the core's PredicateSource.
    let predicate_source = match params.predicate_source {
        PredicateSourceArg::Wp => PredicateSource::WeakestPrecondition,
        PredicateSourceArg::Craig => PredicateSource::CraigInterpolation,
    };

    // R.2.5b session-1 follow-up — map CLI MustEdgeInferenceArg to
    // the core MustEdgeInference enum.
    let must_edge_inference = match params.must_edge_inference {
        MustEdgeInferenceArg::Auto | MustEdgeInferenceArg::Off => {
            mununu_core::adapter::btor2::kmts_lift::MustEdgeInference::Off
        }
        MustEdgeInferenceArg::SmtPerTarget => {
            mununu_core::adapter::btor2::kmts_lift::MustEdgeInference::SmtPerTarget
        }
        MustEdgeInferenceArg::SmtPerTargetStandard => {
            mununu_core::adapter::btor2::kmts_lift::MustEdgeInference::SmtPerTargetStandard
        }
        MustEdgeInferenceArg::SmtHyperMust => {
            mununu_core::adapter::btor2::kmts_lift::MustEdgeInference::SmtHyperMust
        }
    };

    // DR1 (2026-06-19) — map CLI MayEdgeInferenceArg to the core enum.
    let may_edge_inference = match params.may_edge_inference {
        MayEdgeInferenceArg::Off => mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
        MayEdgeInferenceArg::SmtAllPairs => {
            mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs
        }
    };

    let cegar_opts = CegarOptions {
        max_iterations: params.max_iterations,
        predicate_source,
        max_cube_count: 1024,
        capture_approximants: false,
        enable_approximant_reuse: false,
        smart_uf_cap: true,
        lift_strategy: LiftStrategy::Eager,
        must_edge_inference,
        may_edge_inference,
        // CTXDSL Phase 2 — capture the final cube model only when the
        // `--emit-ctxdsl` flag asks for it.
        emit_ctxdsl: params.emit_ctxdsl.is_some(),
    };

    // R-S8 session 2 (2026-06-08) — parse the `--init-values`
    // CLI flag(s) into a synthetic sidecar JSON that exercises the
    // shipped `sidecar_config_values` resolver bridge end-to-end.
    // Format: `REG=v1,v2,v3`. Builds an `SvAnnotation` with one
    // signal per flag entry; the CEGAR loop reads `sidecar_json`
    // via the bridge and threads `config_values` into the
    // predicate-cube lift.
    let mut adapter_options = build_adapter_options_with_config_values(params.config_values)?;
    // R.6.6 / V.6 (2026-06-09) — thread the `--controllable`
    // CLI flag values into `AdapterOptions::controllable_inputs`,
    // which the predicate-cube lifter reads to partition boolean
    // inputs into env / ctrl classes + emit per-combo dual-label
    // transitions with the appropriate `LabelControllability` tags.
    adapter_options.controllable_inputs = params.controllable_inputs.to_vec();
    // R-S2b.6 / P1 (§Phase 11 slot-3 close follow-up, 2026-06-12)
    // — thread the `--sv-source` CLI flag value into
    // `AdapterOptions::sv_source_path`, which the bit-blaster's
    // `apply_simulate_reset_seeding` orchestration reads to
    // trigger the Verilator reset simulation when the sidecar
    // declares a `simulate_reset` block. Closes the parity gap
    // surfaced by the slot-3 close cadence checkpoint at
    // .claude/reviews/slot-3-close-cadence-2026-06-12.md (R-S2b.6
    // unreachable from CLI before this wire-in).
    adapter_options.sv_source_path = params.sv_source.map(|p| p.to_path_buf());

    // R-S6.6 / P2 (§Phase 11 slot-3 close follow-up, 2026-06-12)
    // — thread the `--sidecar` CLI flag into
    // `AdapterOptions::sidecar_path` AND override the synthetic
    // `sidecar_json` with the file's contents when provided. The
    // bit-blaster's `apply_vcd_trace_seeding` orchestration reads
    // `sidecar_path.parent()` to resolve relative `vcd_traces`
    // paths declared in the sidecar. Closes the second parity gap
    // surfaced by the slot-3 close cadence checkpoint (R-S6.6
    // unreachable from CLI before this wire-in).
    //
    // When both `--sidecar` and `--init-values` are set, the
    // file-based sidecar wins (the file is the authoritative
    // schema source; `--init-values` is the synth-sidecar
    // convenience flag for hand-tuning).
    if let Some(sidecar_path) = params.sidecar {
        let sidecar_content = std::fs::read_to_string(sidecar_path)
            .map_err(|e| format!("Failed to read sidecar '{}': {e}", sidecar_path.display()))?;
        adapter_options.sidecar_json = Some(sidecar_content);
        adapter_options.sidecar_path = Some(sidecar_path.to_path_buf());
    }

    let trace = cegar_refine_loop(
        &formula,
        content,
        initial_predicates,
        &env,
        &adapter_options,
        &cegar_opts,
    )
    .map_err(|e| format!("cegar refine loop: {}", e.message))?;

    // CTXDSL Phase 2 (2026-06-22) — opt-in model + formula CTXDSL dump.
    // When `--emit-ctxdsl <PATH>` is set, the loop captured the final
    // refined cube `Clts` into `trace.final_clts`; serialize it together
    // with the checked formula (the original `--formula` string) and write
    // the document to PATH. Uses stderr for the confirmation so the
    // `--json` verdict on stdout stays clean.
    if let Some(emit_path) = params.emit_ctxdsl {
        match &trace.final_clts {
            Some(clts) => {
                let model_ctxdsl = mununu_core::adapter::clts_to_ir::clts_to_ctxdsl_with_formula(
                    clts,
                    "lifted_kmts",
                    "cegar_model",
                    "checked_property",
                    params.formula,
                )
                .map_err(|e| format!("emit ctxdsl: {}", e.message))?;
                std::fs::write(emit_path, &model_ctxdsl)
                    .map_err(|e| format!("write ctxdsl to '{}': {e}", emit_path.display()))?;
                eprintln!(
                    "Wrote model + formula CTXDSL to {} ({} bytes)",
                    emit_path.display(),
                    model_ctxdsl.len()
                );
            }
            None => {
                eprintln!(
                    "warning: --emit-ctxdsl set but the CEGAR loop produced no final cube model"
                );
            }
        }
    }

    // #2 (M.4) — surface the final 3-valued verdict the loop reached.
    // Previously the CLI printed only `terminated_with` + the predicate
    // count, so a `Converged` run gave no T/F/⊥ polarity (the API
    // handler already exposes the cell counts; this mirrors it on the
    // CLI). The verdict is over the lifted KMTS's cube cells: `false`
    // cells falsify the formula (e.g. a reachable unmatched encoding),
    // `unknown` cells are still indefinite, `true` cells satisfy it.
    // A4 — unlifted cells (unsatisfiable, or not reachable from the initial cubes) are ⊥ by
    // construction and are reported as their own count, never as "needs refinement".
    let (t_cells, f_cells, bot_cells, unlifted_cells) =
        mununu_core::adapter::btor2::cegar::tally_cells(
            &trace.final_verdict,
            &trace.unlifted_cells,
        );
    let outcome = if bot_cells > 0 {
        format!("INDEFINITE — {bot_cells} cell(s) need further refinement")
    } else if f_cells > 0 {
        format!("PROPERTY VIOLATED — {f_cells} cell(s) falsify the formula")
    } else {
        format!("PROPERTY HOLDS — all {t_cells} cell(s) satisfy the formula")
    };

    // Track I.1 (2026-06-24) — make a non-HOLDS verdict actionable: surface the
    // predicate valuations of the cube cells that falsify the formula, or that
    // the abstraction cannot decide. Capped so a large cube does not flood the
    // output; the count above (`f_cells` / `bot_cells`) is the full total.
    const WITNESS_CAP: usize = 4;
    let violating_cells = trace.violating_cells(WITNESS_CAP);
    let undecided_cells = trace.undecided_cells(WITNESS_CAP);

    if params.json {
        let summary = serde_json::json!({
            "fixture": fixture_label,
            "formula": params.formula,
            "predicate_source": format!("{:?}", params.predicate_source),
            "iterations": trace.iterations.len(),
            "terminated_with": format!("{:?}", trace.terminated_with),
            "final_predicate_count": trace.final_predicates.len(),
            "cells": {
                "true_cells": t_cells,
                "false_cells": f_cells,
                "unknown_cells": bot_cells,
                "unlifted_cells": unlifted_cells,
            },
            "outcome": outcome,
            "violating_cells": cegar_cells_json(&violating_cells),
            "undecided_cells": cegar_cells_json(&undecided_cells),
            // Track I.1 (trace slice) — reachability countertrace for a
            // violated verdict (null when the property is not violated at init).
            "trace": trace
                .counterexample
                .as_ref()
                .map(mununu_core::api::models::Trace::from_cegar),
            // Track I.1 (undecided-explanation) — load-bearing registers for ⊥ cells.
            "refinement_candidates": trace.init_refinement_candidates,
            "approximant_reuse_enabled": trace.approximant_reuse_enabled,
            "lazy_lift_pending": trace.lazy_lift_pending,
            "warnings": trace.warnings.iter().map(|w| w.message.clone()).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .map_err(|e| format!("serialize summary: {e}"))?
        );
    } else {
        println!("CEGAR refinement loop completed");
        println!("  fixture:           {fixture_label}");
        println!("  formula:           {}", params.formula);
        println!("  predicate_source:  {:?}", params.predicate_source);
        println!("  iterations:        {}", trace.iterations.len());
        println!("  terminated_with:   {:?}", trace.terminated_with);
        println!("  final predicates:  {}", trace.final_predicates.len());
        println!("  verdict cells:     T={t_cells} F={f_cells} ⊥={bot_cells}");
        println!("  outcome:           {outcome}");
        // Track I.1 — which cube valuations falsify / can't be decided.
        if bot_cells > 0 && !undecided_cells.is_empty() {
            println!("  undecided at:");
            for w in &undecided_cells {
                println!("    - {{{}}}", w.render());
            }
            if bot_cells > undecided_cells.len() {
                println!("    … and {} more", bot_cells - undecided_cells.len());
            }
        } else if f_cells > 0 && !violating_cells.is_empty() {
            println!("  falsified at:");
            for w in &violating_cells {
                println!("    - {{{}}}", w.render());
            }
            if f_cells > violating_cells.len() {
                println!("    … and {} more", f_cells - violating_cells.len());
            }
        }
        // Track I.1 (trace slice) — the reachability path from an initial
        // failing cell to a trap (or the farthest reachable failing cell).
        if let Some(ct) = &trace.counterexample {
            let n = ct.steps.len();
            println!(
                "  counterexample trace ({n} step{}{}):",
                if n == 1 { "" } else { "s" },
                if ct.ends_in_trap {
                    ", ends in trap"
                } else {
                    ""
                }
            );
            for (i, w) in ct.steps.iter().enumerate() {
                println!("    {i}. {{{}}}", w.render());
            }
        }
        // Track I.1 (undecided-explanation) — when ⊥ cells remain, name the
        // registers the failure subgame flagged as load-bearing; adding
        // predicates over them (or promoting their init policy) may resolve it.
        if bot_cells > 0 && !trace.init_refinement_candidates.is_empty() {
            println!(
                "  undecided because the abstraction can't decide {bot_cells} cell(s); \
                 try predicates over: {}",
                trace.init_refinement_candidates.join(", ")
            );
        }
        if !trace.warnings.is_empty() {
            println!("  warnings:");
            for w in &trace.warnings {
                println!("    - {}", w.message);
            }
        }
    }

    Ok(())
}

fn btor2_discover(args: Btor2DiscoverArgs) -> Result<(), String> {
    use mununu_core::adapter::sidecar::predicate_image::{
        ImageOptions, all_smt::discover_values_for_btor2_file,
    };
    use mununu_core::adapter::systemverilog::annotation::{
        DiscoveredValues, SvAnnotation, merge_discovered_values,
    };

    if !args.file.exists() {
        return Err(format!(
            "BTOR2 input file does not exist: {}",
            args.file.display()
        ));
    }
    let src = fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read '{}': {e}", args.file.display()))?;
    let file = mununu_core::adapter::btor2::parser::parse(&src)
        .map_err(|e| format!("BTOR2 parse error in '{}': {e}", args.file.display()))?;

    let opts = ImageOptions {
        cap_edges: args.cap_edges,
        ..ImageOptions::default()
    };

    eprintln!(
        "Running predicate-image discovery on {} (cap_edges={})...",
        args.file.display(),
        args.cap_edges
    );
    let results = discover_values_for_btor2_file(&file, &opts)
        .map_err(|e| format!("predicate-image discovery failed: {e}"))?;

    if results.is_empty() {
        eprintln!("No discovered values for any state cell.");
    } else {
        for (signal, discovered) in &results {
            eprintln!("  {} — {} value(s):", signal, discovered.values.len());
            for v in &discovered.values {
                let from = v.from.as_deref().unwrap_or("predicate-image");
                eprintln!("    {} = {} ({})", v.name, v.value, from);
            }
        }
    }

    // Resolve the output sidecar path: explicit `--output`, or
    // `<stem>.mununu.json` next to the input.
    let sidecar_path = match &args.output {
        Some(p) => p.clone(),
        None => {
            let stem = args
                .file
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| {
                    format!("Cannot derive sidecar stem from '{}'", args.file.display())
                })?;
            args.file.with_file_name(format!("{stem}.mununu.json"))
        }
    };

    // Load existing sidecar if present, else start from a minimal
    // annotation shape. Either way we merge in the discovered values.
    let mut annotation: SvAnnotation = if sidecar_path.exists() {
        let body = fs::read_to_string(&sidecar_path).map_err(|e| {
            format!(
                "Failed to read existing sidecar '{}': {e}",
                sidecar_path.display()
            )
        })?;
        serde_json::from_str(&body).map_err(|e| {
            format!(
                "Failed to parse existing sidecar '{}': {e}",
                sidecar_path.display()
            )
        })?
    } else {
        SvAnnotation {
            schema: Some("mununu_sv_annotation_v1".to_string()),
            module: args
                .file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("design")
                .to_string(),
            source: args
                .file
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string()),
            signals: Vec::new(),
            inputs: Vec::new(),
            controllable: Vec::new(),
            properties: Vec::new(),
            discovered_values: std::collections::HashMap::new(),
            parameters: std::collections::HashMap::new(),
            parameter_concretizations: std::collections::HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        }
    };

    let results_typed: std::collections::HashMap<String, DiscoveredValues> = results;
    merge_discovered_values(&mut annotation.discovered_values, results_typed);

    let json = serde_json::to_string_pretty(&annotation)
        .map_err(|e| format!("Failed to serialise updated sidecar: {e}"))?;
    fs::write(&sidecar_path, json)
        .map_err(|e| format!("Failed to write sidecar '{}': {e}", sidecar_path.display()))?;
    eprintln!("Updated sidecar: {}", sidecar_path.display());

    Ok(())
}

fn handle_context(command: ContextCommand) -> Result<(), String> {
    match command {
        ContextCommand::Merge(args) => context_merge(args),
        ContextCommand::Summarize(args) => context_summarize(args),
        ContextCommand::Predicates(args) => context_predicates(args),
        ContextCommand::Eval(args) => context_eval(args),
        ContextCommand::Synth(args) => context_synthesize(args),
        ContextCommand::Graph(args) => context_graph(args),
    }
}

fn handle_extraction(command: ExtractionCommand) -> Result<(), String> {
    match command {
        ExtractionCommand::Validate(args) => extraction_validate(args),
        ExtractionCommand::Check(args) => extraction_check(args),
    }
}

fn handle_contract(command: ContractCommand) -> Result<(), String> {
    match command {
        ContractCommand::Validate(args) => contract_validate(args),
        ContractCommand::Gaps(args) => contract_gaps(args),
        ContractCommand::Discover(args) => contract_discover(args),
        ContractCommand::Sidecars(args) => contract_sidecars(args),
        ContractCommand::Query(args) => contract_query(args),
        ContractCommand::Review(args) => contract_review(args),
    }
}

fn handle_codesign(command: CodesignCommand) -> Result<(), String> {
    match command {
        CodesignCommand::EmitCmsisHeader(args) => codesign_emit_cmsis_header(args),
        CodesignCommand::Couple(args) => codesign_couple(args),
        CodesignCommand::EmitChaoticStub(args) => codesign_emit_chaotic_stub(args),
        CodesignCommand::Verify(args) => codesign_verify(args),
        CodesignCommand::ImportSvd(args) => codesign_import_svd(args),
        CodesignCommand::ExtractC(args) => codesign_extract_c(args),
        CodesignCommand::ReconcileLabels(args) => codesign_reconcile_labels(args),
    }
}

fn codesign_reconcile_labels(args: CodesignReconcileLabelsArgs) -> Result<(), String> {
    use mununu_core::codesign::reconcile::{
        ReconcileError, peripheral_labels_from_register_map, reconcile_label_alphabets,
    };
    use mununu_core::codesign::register_map::RegisterMap;
    use std::collections::BTreeSet;

    let load_label_list = |path: &PathBuf, side: &str| -> Result<BTreeSet<String>, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("failed to read {side} labels {}: {e}", path.display()))?;
        let labels: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "failed to parse {side} labels {} as JSON array of strings: {e}",
                path.display()
            )
        })?;
        Ok(labels.into_iter().collect())
    };

    let firmware = load_label_list(&args.firmware_labels, "firmware")?;

    let peripheral: BTreeSet<String> = match (
        &args.peripheral_labels,
        &args.peripheral_register_map,
    ) {
        (Some(_), Some(_)) => {
            return Err(
                "pass exactly one of <PERIPHERAL_JSON> or --peripheral-register-map, not both"
                    .to_string(),
            );
        }
        (None, None) => {
            return Err(
                "missing peripheral source: pass <PERIPHERAL_JSON> as the second positional arg or --peripheral-register-map <JSON>"
                    .to_string(),
            );
        }
        (Some(p), None) => load_label_list(p, "peripheral")?,
        (None, Some(rm_path)) => {
            let bytes = std::fs::read(rm_path)
                .map_err(|e| format!("failed to read register-map {}: {e}", rm_path.display()))?;
            let rm: RegisterMap = serde_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "failed to parse register-map {} as JSON: {e}",
                    rm_path.display()
                )
            })?;
            let issues = rm.validate();
            if !issues.is_empty() {
                for issue in &issues {
                    eprintln!("register-map validation: {issue}");
                }
                return Err(format!(
                    "register-map {} has {} validation issue(s) — refusing to proceed",
                    rm_path.display(),
                    issues.len()
                ));
            }
            peripheral_labels_from_register_map(&rm)
        }
    };

    let result = reconcile_label_alphabets(&firmware, &peripheral);

    match args.format.as_str() {
        "json" => match &result {
            Ok(r) => {
                let payload = serde_json::json!({
                    "shared": r.shared,
                    "mismatch": serde_json::Value::Null,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| format!("serialize: {e}"))?
                );
                Ok(())
            }
            Err(ReconcileError::Mismatch(m)) => {
                let payload = serde_json::json!({
                    "shared": Vec::<String>::new(),
                    "mismatch": {
                        "firmware_only": m.firmware_only,
                        "peripheral_only": m.peripheral_only,
                    },
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| format!("serialize: {e}"))?
                );
                Err("label-alphabet mismatch".to_string())
            }
        },
        "human" => match &result {
            Ok(r) => {
                println!("alphabets reconcile ({} shared labels):", r.shared.len());
                for label in &r.shared {
                    println!("  - {label}");
                }
                Ok(())
            }
            Err(ReconcileError::Mismatch(m)) => {
                eprintln!("error: label-alphabet mismatch");
                if !m.firmware_only.is_empty() {
                    eprintln!("  firmware-only ({}):", m.firmware_only.len());
                    for label in &m.firmware_only {
                        eprintln!("    - {label}");
                    }
                }
                if !m.peripheral_only.is_empty() {
                    eprintln!("  peripheral-only ({}):", m.peripheral_only.len());
                    for label in &m.peripheral_only {
                        eprintln!("    - {label}");
                    }
                }
                Err("label-alphabet mismatch".to_string())
            }
        },
        other => Err(format!("unknown --format '{other}' (valid: human, json)")),
    }
}

fn codesign_extract_c(args: CodesignExtractCArgs) -> Result<(), String> {
    use mununu_core::codesign::c_extract_llvm::{LlvmExtractOptions, extract_c_via_llvm};
    use mununu_core::codesign::register_map::RegisterMap;

    let register_map = match args.register_map.as_ref() {
        Some(path) => {
            let bytes = std::fs::read(path)
                .map_err(|e| format!("failed to read register-map {}: {e}", path.display()))?;
            let rm: RegisterMap = serde_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "failed to parse register-map {} as JSON: {e}",
                    path.display()
                )
            })?;
            let issues = rm.validate();
            if !issues.is_empty() {
                for issue in &issues {
                    eprintln!("register-map validation: {issue}");
                }
                return Err(format!(
                    "register-map {} has {} validation issue(s) \u{2014} refusing to proceed",
                    path.display(),
                    issues.len()
                ));
            }
            Some(rm)
        }
        None => {
            if args.synthesize_automaton {
                return Err("--synthesize-automaton requires --register-map <JSON>".to_string());
            }
            None
        }
    };

    // Phase L8: when `--cmsis-stubs` is set, prepend the bundled
    // `cmsis-stubs/` directory (vendor-neutral CMSIS shims) to the
    // include-paths list. The directory ships inside the
    // mununu-core crate; we resolve it relative to the binary's
    // workspace root via the CARGO_MANIFEST_DIR-equivalent
    // environment variable, with a fallback to the conventional
    // path so this also works for installed binaries.
    let mut include_paths = args.include_paths;
    if args.cmsis_stubs {
        include_paths.insert(0, locate_cmsis_stubs());
    }

    let opts = LlvmExtractOptions {
        clang_path: args.clang,
        include_paths,
        defines: args.defines,
        extra_clang_args: args.extra_clang_args,
        register_map,
        synthesize_automaton: args.synthesize_automaton,
        driver_mode: args.driver_mode,
    };
    let extraction =
        extract_c_via_llvm(&args.file, &opts).map_err(|e| format!("C extraction failed: {e}"))?;
    if args.strict && !extraction.warnings.is_empty() {
        for w in &extraction.warnings {
            eprintln!("warning: {w}");
        }
        return Err(format!(
            "--strict: {} extraction warning(s) \u{2014} refusing to proceed",
            extraction.warnings.len()
        ));
    }
    for w in &extraction.warnings {
        eprintln!("warning: {w}");
    }
    let json = serde_json::to_string_pretty(&extraction)
        .map_err(|e| format!("failed to serialise extraction: {e}"))?;
    println!("{json}");
    Ok(())
}

fn codesign_emit_cmsis_header(args: CodesignEmitCmsisHeaderArgs) -> Result<(), String> {
    use mununu_core::codesign::cmsis_emit::{CmsisEmitOptions, emit_cmsis_header};
    use mununu_core::codesign::register_map::RegisterMap;
    use mununu_core::codesign::svd_import::import_svd;

    let owned_maps: Vec<RegisterMap> = match (&args.svd, &args.register_map) {
        (Some(svd), None) => {
            let body = std::fs::read_to_string(svd)
                .map_err(|e| format!("failed to read {}: {e}", svd.display()))?;
            let import = import_svd(&body).map_err(|e| format!("SVD import failed: {e}"))?;
            import.maps
        }
        (None, Some(rm_path)) => {
            let body = std::fs::read_to_string(rm_path)
                .map_err(|e| format!("failed to read {}: {e}", rm_path.display()))?;
            let rm: RegisterMap = serde_json::from_str(&body)
                .map_err(|e| format!("failed to parse register-map JSON: {e}"))?;
            vec![rm]
        }
        (Some(_), Some(_)) => {
            return Err("specify either --svd or --register-map, not both".to_string());
        }
        (None, None) => {
            return Err("--svd <FILE> or --register-map <JSON> is required".to_string());
        }
    };
    let maps: Vec<&RegisterMap> = match &args.peripheral {
        Some(name) => owned_maps
            .iter()
            .filter(|rm| &rm.peripheral == name)
            .collect(),
        None => owned_maps.iter().collect(),
    };
    if maps.is_empty() {
        return Err(match args.peripheral {
            Some(name) => format!("peripheral `{name}` not found"),
            None => "no peripherals in input".to_string(),
        });
    }
    let options = CmsisEmitOptions {
        vendor_prefix: &args.vendor_prefix,
        struct_type_name: None,
    };
    for rm in &maps {
        print!("{}", emit_cmsis_header(rm, &options));
        println!();
    }
    Ok(())
}

/// Phase L8: resolve the bundled `cmsis-stubs/` directory's path.
/// Tries the workspace-relative path first (dev builds running from
/// source), then a `share/mununu/cmsis-stubs` fallback (installed
/// binaries). Returns the first existing directory.
fn locate_cmsis_stubs() -> PathBuf {
    let candidates: &[PathBuf] = &[
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("mununu-core/cmsis-stubs"),
        PathBuf::from("crates/mununu-core/cmsis-stubs"),
        PathBuf::from("../share/mununu/cmsis-stubs"),
    ];
    for candidate in candidates {
        if candidate.is_dir() {
            return candidate.clone();
        }
    }
    // Fallback: the workspace-relative path even if it doesn't
    // exist (clang will fail with a clear "include not found" if
    // so; the user can override via --include).
    candidates[0].clone()
}

fn codesign_import_svd(args: CodesignImportSvdArgs) -> Result<(), String> {
    use mununu_core::codesign::svd_import::import_svd;

    let body = std::fs::read_to_string(&args.svd)
        .map_err(|e| format!("failed to read {}: {e}", args.svd.display()))?;
    let import = import_svd(&body).map_err(|e| format!("SVD import failed: {e}"))?;

    // Emit warnings to stderr so they're visible regardless of stdout
    // capture. Under --strict, any warning is a hard error.
    for w in &import.warnings {
        eprintln!("warning: {w}");
    }
    if args.strict && !import.warnings.is_empty() {
        return Err(format!(
            "--strict: {} SVD warning(s) — refusing to proceed",
            import.warnings.len()
        ));
    }

    // Filter by --peripheral if requested.
    let filtered: Vec<_> = match &args.peripheral {
        Some(name) => import
            .maps
            .into_iter()
            .filter(|m| m.peripheral == *name)
            .collect(),
        None => import.maps,
    };

    if filtered.is_empty() {
        return Err(match args.peripheral {
            Some(name) => format!("no peripheral named `{name}` in {}", args.svd.display()),
            None => format!(
                "SVD file {} contains no importable peripherals",
                args.svd.display()
            ),
        });
    }

    // Decide output mode: --out-dir writes files; default writes JSON
    // to stdout (one peripheral or a JSON array if multiple).
    if let Some(dir) = &args.out_dir {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        for map in &filtered {
            let path = dir.join(format!("{}.json", sanitize_filename(&map.peripheral)));
            let json = serde_json::to_string_pretty(map)
                .map_err(|e| format!("failed to serialise {}: {e}", map.peripheral))?;
            std::fs::write(&path, json)
                .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
            eprintln!("wrote {} → {}", map.peripheral, path.display());
        }
        eprintln!(
            "imported {} peripheral(s) from {}",
            filtered.len(),
            args.svd.display()
        );
    } else if filtered.len() == 1 {
        let json = serde_json::to_string_pretty(&filtered[0])
            .map_err(|e| format!("failed to serialise: {e}"))?;
        println!("{json}");
    } else {
        let json = serde_json::to_string_pretty(&filtered)
            .map_err(|e| format!("failed to serialise: {e}"))?;
        println!("{json}");
    }

    Ok(())
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn codesign_verify(args: CodesignVerifyArgs) -> Result<(), String> {
    use mununu_core::codesign::compose::{ComposeOptions, compose_codesign_ctxdsl};
    use mununu_core::codesign::register_map::RegisterMap;

    let rm_body = std::fs::read_to_string(&args.register_map)
        .map_err(|e| format!("failed to read {}: {e}", args.register_map.display()))?;
    let rm: RegisterMap = serde_json::from_str(&rm_body)
        .map_err(|e| format!("failed to parse register-map JSON: {e}"))?;

    let firmware_text = std::fs::read_to_string(&args.firmware)
        .map_err(|e| format!("failed to read {}: {e}", args.firmware.display()))?;

    let opts = ComposeOptions {
        peripheral_automaton: args.peripheral_automaton.as_deref(),
        composition_name: args.composition_name.as_deref(),
        firmware_members_override: None,
    };
    let composed = compose_codesign_ctxdsl(&rm, &firmware_text, &opts)
        .map_err(|e| format!("codesign compose failed: {e}"))?;

    if let Some(out_path) = &args.emit_ctxdsl {
        std::fs::write(out_path, &composed.ctxdsl)
            .map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;
        eprintln!("wrote composed CTXDSL to {}", out_path.display());
    }

    let automaton_name = args
        .automaton
        .clone()
        .unwrap_or_else(|| composed.composition_name.clone());

    // Parse + realise the composed document so we can evaluate the
    // user's formula. Reuses the same path the `context eval`
    // subcommand uses.
    let context_doc = parse_context_doc(&composed.ctxdsl).map_err(|e| {
        format!("composed CTXDSL failed to parse (this is a bug in codesign::compose): {e:?}")
    })?;
    let sidecar_docs: Vec<ContextDoc> = Vec::new();
    let realized = realize_documents(&context_doc, &sidecar_docs)?;
    let formula = realized
        .formulas
        .get(&args.formula)
        .ok_or_else(|| format!("unknown formula '{}' in composed context", args.formula))?;
    let clts = realized.context.clts(&automaton_name).ok_or_else(|| {
        format!(
            "unknown automaton/composition '{automaton_name}' in composed context — expected one of: {}",
            realized.context.clts_names().join(", ")
        )
    })?;

    let env = realized.environment_for(&automaton_name);
    let options = mununu_core::mu_calculus::EvaluationOptions::default();
    let result =
        mununu_core::mu_calculus::evaluate_with_options(&formula.formula, clts, &env, &options)
            .map_err(|err| format!("μ-calculus evaluation failed: {err}"))?;

    let total_states = clts.state_count();
    let satisfying_count = (0..total_states)
        .filter(|i| result.get(*i).map(|b| *b).unwrap_or(false))
        .count();
    let initial_states: Vec<String> = clts
        .initial_states()
        .iter()
        .filter_map(|sid| clts.state_name(*sid).map(|s| s.to_string()))
        .collect();
    let initial_satisfying: Vec<String> = clts
        .initial_states()
        .iter()
        .filter_map(|sid| {
            if result.get(sid.index()).map(|bit| *bit).unwrap_or(false) {
                clts.state_name(*sid).map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    let all_initials_satisfying = initial_satisfying.len() == initial_states.len();

    if args.json {
        let body = serde_json::json!({
            "register_map": {
                "peripheral": rm.peripheral,
                "base_address": rm.base_address,
                "registers": rm.registers.len(),
            },
            "composition": {
                "automaton": automaton_name,
                "peripheral_automaton": composed.peripheral_automaton,
                "firmware_members": composed.firmware_members,
            },
            "outcome": if all_initials_satisfying { "holds" } else { "violated" },
            "property": {
                "formula": args.formula,
                "automaton": automaton_name,
                "total_states": total_states,
                "satisfying_states": satisfying_count,
                "initial_states": initial_states,
                "initial_satisfying": initial_satisfying,
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!(
        "codesign verify — peripheral `{}` (base {}), composition `{}`",
        rm.peripheral, rm.base_address, automaton_name
    );
    println!(
        "  composed with firmware member(s): {}",
        composed.firmware_members.join(", ")
    );
    println!();
    println!("  formula `{}` over `{automaton_name}`", args.formula);
    println!("    states satisfying: {satisfying_count}/{total_states}");
    println!(
        "    initial states satisfying: {}/{}",
        initial_satisfying.len(),
        initial_states.len()
    );
    if !initial_states.is_empty() {
        println!("      initials: {}", initial_states.join(", "));
        if !initial_satisfying.is_empty() {
            println!("      satisfying: {}", initial_satisfying.join(", "));
        }
    }
    if all_initials_satisfying {
        println!("    verdict: HOLDS");
    } else {
        println!("    verdict: VIOLATED at initial state(s)");
    }
    Ok(())
}

fn codesign_emit_chaotic_stub(args: CodesignEmitChaoticStubArgs) -> Result<(), String> {
    use mununu_core::codesign::coupling::{CouplingOptions, emit_chaotic_stub_ctxdsl};
    use mununu_core::codesign::register_map::RegisterMap;

    let body = std::fs::read_to_string(&args.register_map)
        .map_err(|e| format!("failed to read {}: {e}", args.register_map.display()))?;
    let map: RegisterMap = serde_json::from_str(&body)
        .map_err(|e| format!("failed to parse register-map JSON: {e}"))?;

    let issues = map.validate();
    if !issues.is_empty() {
        for issue in &issues {
            eprintln!("warning: {issue}");
        }
        if args.strict {
            return Err(format!(
                "--strict: {} register-map issue(s) — refusing to proceed",
                issues.len()
            ));
        }
    }

    let opts = CouplingOptions {
        peripheral_automaton: args.peripheral_automaton.as_deref(),
        ..Default::default()
    };
    let stub = emit_chaotic_stub_ctxdsl(&map, &opts);

    match args.output {
        Some(path) => std::fs::write(&path, &stub)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?,
        None => print!("{stub}"),
    }
    Ok(())
}

fn codesign_couple(args: CodesignCoupleArgs) -> Result<(), String> {
    use mununu_core::codesign::coupling::{CouplingOptions, emit_coupling_fragment};
    use mununu_core::codesign::register_map::RegisterMap;

    let body = std::fs::read_to_string(&args.register_map)
        .map_err(|e| format!("failed to read {}: {e}", args.register_map.display()))?;
    let map: RegisterMap = serde_json::from_str(&body)
        .map_err(|e| format!("failed to parse register-map JSON: {e}"))?;

    let issues = map.validate();
    if !issues.is_empty() {
        for issue in &issues {
            eprintln!("warning: {issue}");
        }
        if args.strict {
            return Err(format!(
                "--strict: {} register-map issue(s) — refusing to proceed",
                issues.len()
            ));
        }
    }

    let firmware_refs: Vec<&str> = args.firmware_members.iter().map(String::as_str).collect();
    let opts = CouplingOptions {
        peripheral_automaton: args.peripheral_automaton.as_deref(),
        composition_name: args.composition_name.as_deref(),
        firmware_members: &firmware_refs,
    };
    let fragment = emit_coupling_fragment(&map, &opts);
    print!("{fragment}");
    Ok(())
}

fn contract_review(args: ContractReviewArgs) -> Result<(), String> {
    use mununu_core::contract::discover::{BlackBoxInterface, DiscoverOptions};
    use mununu_core::contract::review::{ProposalCounts, build_review_package};
    use mununu_core::corpus::Corpus;

    let body = std::fs::read_to_string(&args.interface)
        .map_err(|e| format!("failed to read {}: {e}", args.interface.display()))?;
    let iface: BlackBoxInterface =
        serde_json::from_str(&body).map_err(|e| format!("failed to parse interface JSON: {e}"))?;

    let corpus = match &args.corpus {
        Some(root) => Some(
            Corpus::load(root)
                .map_err(|e| format!("failed to load corpus at {}: {e}", root.display()))?,
        ),
        None => None,
    };

    let opts = DiscoverOptions {
        force_controllable: &[],
        force_uncontrollable: &[],
        emit_fairness_gap: false,
        corpus: corpus.as_ref(),
    };
    let pkg = build_review_package(&iface, &opts);
    let counts = ProposalCounts::from_package(&pkg);

    if args.json {
        let rendered = serde_json::to_string_pretty(&pkg)
            .map_err(|e| format!("failed to serialise package: {e}"))?;
        println!("{rendered}");
        return Ok(());
    }

    println!(
        "review for module `{}` — {} proposal(s) [{} assume, {} guarantee, {} corpus]",
        pkg.module,
        counts.total(),
        counts.source_comment_assumptions,
        counts.source_comment_guarantees,
        counts.corpus_references,
    );
    println!(
        "  alphabet: {} label(s), {} gap marker(s)",
        pkg.phase1.labels.len(),
        pkg.phase1.gaps.len(),
    );
    if pkg.proposed_clauses.is_empty() {
        println!(
            "  (no proposals — interface has no @mununu_* clauses or resolved corpus entries)"
        );
        return Ok(());
    }
    for (i, p) in pkg.proposed_clauses.iter().enumerate() {
        println!();
        println!("[{}] {} ({} on {})", i + 1, p.id, p.kind, p.owner,);
        if let Some(desc) = &p.description {
            println!("    formula : {desc}");
        }
        println!(
            "    source  : {}",
            render_proposal_provenance(&p.provenance)
        );
        if let Some(note) = &p.soundness_note {
            println!("    note    : {note}");
        }
    }
    Ok(())
}

fn contract_query(args: ContractQueryArgs) -> Result<(), String> {
    use mununu_core::corpus::Corpus;
    use std::collections::BTreeMap;

    let (domain, name) = match args.id.split_once('/') {
        Some((d, n)) if !d.is_empty() && !n.is_empty() => (d, n),
        _ => {
            return Err(format!(
                "expected DOMAIN/NAME, got '{}' — example: rtl_protocol/axi4_slave",
                args.id
            ));
        }
    };

    let mut params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for raw in &args.params {
        let (key, value_str) = raw
            .split_once('=')
            .ok_or_else(|| format!("--param expects KEY=VALUE, got '{raw}'"))?;
        let value: serde_json::Value = serde_json::from_str(value_str)
            .or_else(|_| Ok::<_, ()>(serde_json::Value::String(value_str.to_string())))
            .unwrap();
        params.insert(key.to_string(), value);
    }

    let corpus = if args.corpus.exists() {
        Corpus::load(&args.corpus)
            .map_err(|e| format!("failed to load corpus from {}: {e}", args.corpus.display()))?
    } else {
        eprintln!(
            "warning: corpus directory '{}' does not exist; using empty corpus",
            args.corpus.display()
        );
        Corpus::empty()
    };

    let hits = corpus.query(domain, name, &params);

    if args.json {
        let body = serde_json::to_string_pretty(&hits)
            .map_err(|e| format!("failed to serialise corpus hits: {e}"))?;
        println!("{body}");
        return Ok(());
    }

    if hits.is_empty() {
        println!("no contract entries match {domain}/{name} with the supplied parameters");
        return Ok(());
    }
    println!(
        "found {} contract candidate(s) for {domain}/{name}:",
        hits.len()
    );
    for (i, entry) in hits.iter().enumerate() {
        let provenance = match &entry.provenance {
            mununu_core::corpus::Provenance::MununuVerified { verified_against } => {
                format!(
                    "mununu-verified ({})",
                    verified_against.as_deref().unwrap_or("no reference")
                )
            }
            mununu_core::corpus::Provenance::Vendor { name, .. } => format!("vendor:{name}"),
            mununu_core::corpus::Provenance::Community { .. } => "community".to_string(),
        };
        println!(
            "  {}. {} @ {}  [{}]",
            i + 1,
            entry.id,
            entry.version,
            provenance,
        );
        if let Some(desc) = &entry.description {
            println!("       {desc}");
        }
    }
    Ok(())
}

fn contract_sidecars(args: ContractSidecarsArgs) -> Result<(), String> {
    use mununu_core::contract::discover::{
        BlackBoxInterface, DiscoverOptions, build_blackbox_sidecars,
    };
    use mununu_core::corpus::Corpus;

    let body = std::fs::read_to_string(&args.interfaces)
        .map_err(|e| format!("failed to read {}: {e}", args.interfaces.display()))?;
    let interfaces: Vec<BlackBoxInterface> = serde_json::from_str(&body)
        .map_err(|e| format!("failed to parse interfaces JSON (expected array): {e}"))?;

    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("failed to create {}: {e}", args.out_dir.display()))?;

    let corpus = match &args.corpus {
        Some(root) => Some(
            Corpus::load(root)
                .map_err(|e| format!("failed to load corpus at {}: {e}", root.display()))?,
        ),
        None => None,
    };

    let opts = DiscoverOptions {
        force_controllable: &[],
        force_uncontrollable: &[],
        emit_fairness_gap: args.emit_fairness_gap,
        corpus: corpus.as_ref(),
    };
    let sidecars = build_blackbox_sidecars(&interfaces, &opts);

    for sidecar in &sidecars {
        let target = args.out_dir.join(&sidecar.filename);
        std::fs::write(&target, &sidecar.content)
            .map_err(|e| format!("failed to write {}: {e}", target.display()))?;
        println!("wrote: {}", target.display());
    }
    println!(
        "wrote {} sidecar(s) for {} black-box module(s)",
        sidecars.len(),
        interfaces.len(),
    );
    Ok(())
}

fn contract_discover(args: ContractDiscoverArgs) -> Result<(), String> {
    use mununu_core::contract::discover::{BlackBoxInterface, DiscoverOptions, discover_phase1};
    use mununu_core::corpus::Corpus;

    let body = std::fs::read_to_string(&args.interface)
        .map_err(|e| format!("failed to read {}: {e}", args.interface.display()))?;
    let iface: BlackBoxInterface =
        serde_json::from_str(&body).map_err(|e| format!("failed to parse interface JSON: {e}"))?;

    let corpus = match &args.corpus {
        Some(root) => Some(
            Corpus::load(root)
                .map_err(|e| format!("failed to load corpus at {}: {e}", root.display()))?,
        ),
        None => None,
    };

    let force_c: Vec<&str> = args.force_controllable.iter().map(|s| s.as_str()).collect();
    let force_u: Vec<&str> = args
        .force_uncontrollable
        .iter()
        .map(|s| s.as_str())
        .collect();
    let opts = DiscoverOptions {
        force_controllable: &force_c,
        force_uncontrollable: &force_u,
        emit_fairness_gap: args.emit_fairness_gap,
        corpus: corpus.as_ref(),
    };
    let output = discover_phase1(&iface, &opts);
    output.gaps.emit_diagnostics();

    if let Some(source) = &args.write_sidecar {
        let target = output
            .gaps
            .write_todo_sidecar(source)
            .map_err(|e| format!("failed to write contract.todo.json: {e}"))?;
        println!("wrote sidecar: {}", target.display());
    }

    if args.json {
        let rendered = serde_json::to_string_pretty(&output)
            .map_err(|e| format!("failed to serialise output: {e}"))?;
        println!("{rendered}");
    } else {
        println!(
            "phase-1 discovery: {} label(s), {} gap marker(s) for module `{}`",
            output.labels.len(),
            output.gaps.len(),
            output.module
        );
        for res in &output.corpus_resolutions {
            match res.status {
                mununu_core::contract::discover::ResolutionStatus::Resolved => {
                    let alt = match res.alternative_matched {
                        Some(true) => format!(
                            " [alt `{}` ok]",
                            res.parsed.alternative.as_deref().unwrap_or("")
                        ),
                        Some(false) => format!(
                            " [alt `{}` MISSING on entry]",
                            res.parsed.alternative.as_deref().unwrap_or("")
                        ),
                        None => String::new(),
                    };
                    println!(
                        "  corpus: resolved `{}` → {}{alt}",
                        res.raw_uri,
                        res.matched_ids.join(", "),
                    );
                }
                mununu_core::contract::discover::ResolutionStatus::NotFound => {
                    println!(
                        "  corpus: `{}` not found ({}/{}{})",
                        res.raw_uri,
                        res.parsed.domain,
                        res.parsed.name,
                        res.parsed
                            .version
                            .as_ref()
                            .map(|v| format!("@{v}"))
                            .unwrap_or_default(),
                    );
                }
                mununu_core::contract::discover::ResolutionStatus::NoCorpus => {
                    println!(
                        "  corpus: `{}` referenced but no corpus supplied (use --corpus)",
                        res.raw_uri,
                    );
                }
                mununu_core::contract::discover::ResolutionStatus::Malformed => {
                    println!("  corpus: `{}` malformed URI", res.raw_uri);
                }
                mununu_core::contract::discover::ResolutionStatus::SidecarReference => {
                    println!(
                        "  corpus: `{}` is a sidecar reference (skipped)",
                        res.raw_uri
                    );
                }
            }
        }
    }

    if args.strict_contracts && output.gaps.is_strict_failure() {
        return Err(format!(
            "--strict-contracts: {} unresolved contract gap(s) — refusing to proceed",
            output.gaps.len()
        ));
    }
    Ok(())
}

fn contract_gaps(args: ContractGapsArgs) -> Result<(), String> {
    use mununu_core::contract::gap::GapMarkerReport;

    let body = std::fs::read_to_string(&args.gap_report)
        .map_err(|e| format!("failed to read {}: {e}", args.gap_report.display()))?;
    let report: GapMarkerReport =
        serde_json::from_str(&body).map_err(|e| format!("failed to parse gap report JSON: {e}"))?;

    report.emit_diagnostics();

    if let Some(source) = &args.write_sidecar {
        let target = report
            .write_todo_sidecar(source)
            .map_err(|e| format!("failed to write contract.todo.json sidecar: {e}"))?;
        println!("wrote sidecar: {}", target.display());
    }

    if args.json {
        let rendered = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("failed to serialise report: {e}"))?;
        println!("{rendered}");
    } else {
        println!(
            "gap report: {} marker(s) across {} module(s)",
            report.len(),
            report.by_module().len(),
        );
    }

    if args.strict_contracts && report.is_strict_failure() {
        return Err(format!(
            "--strict-contracts: {} unresolved contract gap(s) — refusing to proceed",
            report.len()
        ));
    }
    Ok(())
}

fn contract_validate(args: ContractValidateArgs) -> Result<(), String> {
    use mununu_core::contract::{ContractSet, discharge};

    let body = std::fs::read_to_string(&args.contract_set)
        .map_err(|e| format!("failed to read {}: {e}", args.contract_set.display()))?;
    let set: ContractSet = serde_json::from_str(&body)
        .map_err(|e| format!("failed to parse contract set JSON: {e}"))?;
    let verdict = discharge::validate(&set);

    if args.json {
        let rendered = serde_json::to_string_pretty(&verdict)
            .map_err(|e| format!("failed to serialise verdict: {e}"))?;
        println!("{rendered}");
        return Ok(());
    }

    render_discharge_verdict_text(&verdict);
    Ok(())
}

fn handle_sv(command: SvCommand) -> Result<(), String> {
    match command {
        SvCommand::Preprocess(args) => sv_preprocess(args),
        SvCommand::EmitBtor2PerModule(args) => sv_emit_btor2_per_module(args),
        SvCommand::Validate(args) => sv_validate(args),
        SvCommand::Discover(args) => sv_discover(args),
        SvCommand::Cegar(args) => sv_cegar(args),
        SvCommand::ExtractSva(args) => sv_extract_sva(args),
        SvCommand::VerifyAuto(args) => sv_verify_auto(*args),
        SvCommand::Verify(args) => sv_verify(args),
        SvCommand::VerifyLiveness(args) => sv_verify_liveness(args),
        SvCommand::VerifyRecoverability(args) => sv_verify_recoverability(args),
        SvCommand::CheckFsm(args) => sv_check_fsm(args),
        SvCommand::Lint(args) => sv_lint(args),
        SvCommand::CdcLint(args) => sv_cdc_lint(args),
        SvCommand::Mutate(args) => sv_mutate(args),
    }
}

/// Read the primary + additional SV sources named by `args` into a core `SvLift`.
fn read_sv_lift(args: &SvLiftArgs) -> Result<mununu_core::adapter::sv_verify::SvLift, String> {
    // E6 — `--design-dir`: discover + auto-assemble a multi-file design.
    if let Some(dir) = &args.design_dir {
        use mununu_core::adapter::yosys::source_manifest;
        let files = source_manifest::discover_sv_files(dir, &args.exclude)?;
        let design_name = dir.file_name().and_then(|s| s.to_str());
        let a = source_manifest::assemble_sv_design(&files, design_name);
        // explicit --top overrides the detected top; --include-dir adds to detected dirs.
        let top = args.top.clone().or(a.top);
        for note in &a.notes {
            // When an explicit `--top` is given it WINS (`args.top.or(a.top)` above), so the
            // auto-detection's "N top candidates → ambiguous"/"no un-instantiated module" notes are
            // moot and actively misleading (they read as "your --top was ignored" when it was not).
            // Suppress just those; keep every other assembly note.
            if args.top.is_some()
                && (note.contains("top candidate") || note.contains("un-instantiated module"))
            {
                continue;
            }
            eprintln!("design-dir: {note}");
        }
        if let Some(t) = &args.top {
            eprintln!("design-dir: explicit --top '{t}' used (overrides any auto-detection)");
        }
        let include_dirs = args
            .include_dirs
            .iter()
            .cloned()
            .chain(a.include_dirs)
            .collect();
        // mununu#475 item 3 — --search-path composes with --design-dir: a
        // primary block from design-dir + sibling libraries discovered
        // under each search-path. Dedup against everything already staged.
        let mut additional_sources = a.additional;
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for dir in &args.search_paths {
            let files = source_manifest::discover_sv_files(dir, &args.exclude)?;
            for (p, content) in files {
                if let Ok(c) = p.canonicalize()
                    && !seen.insert(c)
                {
                    continue;
                }
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("source.sv")
                    .to_string();
                // Avoid a name collision with an already-staged source (rare;
                // additive collisions have no lift value).
                if additional_sources.iter().any(|(n, _)| n == &name) {
                    continue;
                }
                additional_sources.push((name, content));
            }
        }
        return Ok(mununu_core::adapter::sv_verify::SvLift {
            primary_name: a.primary.0.clone(),
            source: a.primary.1,
            additional_sources,
            top,
            use_sv2v: args.frontend.sv2v(),
            merge_clock_domains: args.merge_clock_domains,
            include_dirs,
            frontend: args.frontend.into(),
            params: mununu_core::adapter::sv_verify::parse_params(&args.params)?,
            cutpoints: args.cutpoint.clone(),
            lift_overrides: read_lift_overrides(&args.lift_override)?,
        });
    }
    let file = args
        .file
        .as_ref()
        .ok_or_else(|| "provide a primary SV_FILE or --design-dir".to_string())?;
    let source = std::fs::read_to_string(file)
        .map_err(|e| format!("Failed to read SV '{}': {e}", file.display()))?;
    let mut additional_sources = Vec::new();
    // Dedup by canonical path so a source listed via both --source and
    // --search-path (or already the primary file) is not double-added.
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Ok(c) = file.canonicalize() {
        seen.insert(c);
    }
    for p in &args.sources {
        if let Ok(c) = p.canonicalize()
            && !seen.insert(c)
        {
            continue;
        }
        let content = std::fs::read_to_string(p)
            .map_err(|e| format!("Failed to read '{}': {e}", p.display()))?;
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("source.sv")
            .to_string();
        additional_sources.push((name, content));
    }
    // mununu#475 item 3 — --search-path: discover .sv/.v under each named
    // directory (honouring --exclude, matching --design-dir's semantics)
    // and add to additional_sources. Dedup TWICE: first by canonical path
    // (the same file appearing via both --source and search-path), then
    // by short filename (a search-path file that name-collides with the
    // primary or an explicit source is a distinct file but would trip a
    // "duplicate module" error under yosys — skip it, honest additive-only
    // behaviour). The design-dir branch above applies the same filename
    // check for the same reason.
    for dir in &args.search_paths {
        use mununu_core::adapter::yosys::source_manifest;
        let files = source_manifest::discover_sv_files(dir, &args.exclude)?;
        for (p, content) in files {
            if let Ok(c) = p.canonicalize()
                && !seen.insert(c)
            {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("source.sv")
                .to_string();
            // Suppress name collisions with the primary (its file_name) or
            // any already-staged additional source. A rare-but-possible case
            // is a search-path directory that mirrors the primary's own
            // filename — better to silently prefer the primary than emit two
            // modules of the same name to yosys.
            let primary_name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name == primary_name || additional_sources.iter().any(|(n, _)| n == &name) {
                continue;
            }
            additional_sources.push((name, content));
        }
    }
    Ok(mununu_core::adapter::sv_verify::SvLift {
        primary_name: file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("top.sv")
            .to_string(),
        source,
        additional_sources,
        top: args.top.clone(),
        use_sv2v: args.frontend.sv2v(),
        merge_clock_domains: args.merge_clock_domains,
        include_dirs: args.include_dirs.clone(),
        frontend: args.frontend.into(),
        params: mununu_core::adapter::sv_verify::parse_params(&args.params)?,
        cutpoints: args.cutpoint.clone(),
        lift_overrides: read_lift_overrides(&args.lift_override)?,
    })
}

impl SvLiftArgs {
    /// Display name for the primary input — the file path, or the assembled
    /// design directory when `--design-dir` is used.
    fn primary_display(&self) -> String {
        match (&self.file, &self.design_dir) {
            (Some(f), _) => f.display().to_string(),
            (None, Some(d)) => format!("{} (design-dir)", d.display()),
            (None, None) => "<none>".to_string(),
        }
    }
}

/// `mununu sv verify-bad` — lift SV and decide `bad`-reachability with the portfolio.
fn sv_verify(args: SvVerifyArgs) -> Result<(), String> {
    use mununu_core::adapter::sv_verify::sv_verify_safety;
    use mununu_core::verdict::PropertyVerdict;

    let file = args.lift.primary_display();
    let outcome = sv_verify_safety(&read_sv_lift(&args.lift)?)?;
    let summary = serde_json::json!({
        "file": file,
        "outcome": PropertyVerdict::from(outcome.verdict).as_str(),
        "reachable_by": outcome.reachable_by,
        "unreachable_by": outcome.unreachable_by,
        "contradiction": outcome.verdict
            == mununu_core::adapter::reach_portfolio::ReachVerdict::Contradiction,
    });
    print_json_summary(&summary)?;
    ci_gate_exit(
        PropertyVerdict::from(outcome.verdict).as_str(),
        args.ci.fail_on,
    );
    Ok(())
}

/// `mununu sv verify-liveness` — lift SV and decide `AG(request → AF grant)`.
/// mununu#541 — `--json` makes an ERROR machine-readable too.
///
/// The success path already printed JSON; a failure printed prose to stderr, so a subprocess
/// consumer had to parse two formats and choose by exit code. The wrapper is deliberately thin —
/// all the work stays in `sv_verify_liveness_impl`, so the only thing this adds is the output contract.
fn sv_verify_liveness(args: SvVerifyLivenessArgs) -> Result<(), String> {
    if !args.responses.is_empty() {
        return sv_verify_liveness_all(SvVerifyLivenessAllArgs {
            lift: args.lift,
            responses: args.responses,
            ci: args.ci,
            json: args.json,
        });
    }
    let as_json = args.json.json;
    let file = args.lift.primary_display();
    match sv_verify_liveness_impl(args) {
        Ok(()) => Ok(()),
        Err(e) if as_json => exit_with_json_error("sv-verify-liveness", &file, &e),
        Err(e) => Err(e),
    }
}

fn sv_verify_liveness_impl(args: SvVerifyLivenessArgs) -> Result<(), String> {
    use mununu_core::adapter::sv_verify::sv_verify_liveness as core_sv_verify_liveness;
    use mununu_core::verdict::PropertyVerdict;

    let file = args.lift.primary_display();
    let (Some(request), Some(grant)) = (args.request.as_deref(), args.grant.as_deref()) else {
        return Err(
            "sv verify-liveness: give --request and --grant, or one or more --response".to_string(),
        );
    };
    let lift = read_sv_lift(&args.lift)?;
    let (verdict, outcome) = if args.fairness.is_empty() {
        core_sv_verify_liveness(&lift, request, grant)?
    } else {
        mununu_core::adapter::sv_verify::sv_verify_liveness_under_fairness(
            &lift,
            request,
            grant,
            &args.fairness,
        )?
    };
    let property = format!("AG(({request}) -> AF ({grant}))");
    let report = mununu_core::api::models::SvVerifyLivenessReport {
        file,
        property: if args.fairness.is_empty() {
            property
        } else {
            let gf: Vec<String> = args.fairness.iter().map(|f| format!("GF ({f})")).collect();
            format!("({}) -> {property}", gf.join(" && "))
        },
        outcome: PropertyVerdict::from(verdict).as_str().to_string(),
        decided_by: outcome
            .reachable_by
            .iter()
            .chain(outcome.unreachable_by.iter())
            .map(|e| e.to_string())
            .collect(),
    };
    print_typed_report(&report)?;
    ci_gate_exit(PropertyVerdict::from(verdict).as_str(), args.ci.fail_on);
    Ok(())
}

/// `mununu sv verify-liveness --response` — lift SV and decide the conjunction
/// `⋀ᵢ AG(aᵢ → AF bᵢ)` from repeatable `--response "ANTE => CONS"` pairs. SV-direct
/// peer of `btor2 verify-liveness --response`; same JSON summary + CI exit.
/// mununu#541 — `--json` makes an ERROR machine-readable too.
///
/// The success path already printed JSON; a failure printed prose to stderr, so a subprocess
/// consumer had to parse two formats and choose by exit code. The wrapper is deliberately thin —
/// all the work stays in `sv_verify_liveness_all_impl`, so the only thing this adds is the output contract.
fn sv_verify_liveness_all(args: SvVerifyLivenessAllArgs) -> Result<(), String> {
    let as_json = args.json.json;
    let file = args.lift.primary_display();
    match sv_verify_liveness_all_impl(args) {
        Ok(()) => Ok(()),
        Err(e) if as_json => exit_with_json_error("sv-verify-liveness", &file, &e),
        Err(e) => Err(e),
    }
}

fn sv_verify_liveness_all_impl(args: SvVerifyLivenessAllArgs) -> Result<(), String> {
    use mununu_core::adapter::liveness_rescue::response_conjunction_property;
    use mununu_core::adapter::sv_verify::sv_verify_liveness_all as core_sv_verify_liveness_all;
    use mununu_core::verdict::PropertyVerdict;

    let file = args.lift.primary_display();
    let (verdict, outcomes) =
        core_sv_verify_liveness_all(&read_sv_lift(&args.lift)?, &args.responses)?;
    let report = mununu_core::api::models::SvVerifyLivenessAllReport {
        file,
        property: response_conjunction_property(&args.responses),
        outcome: PropertyVerdict::from(verdict).as_str().to_string(),
        responses: per_response_decided_by(&args.responses, &outcomes),
    };
    print_typed_report(&report)?;
    ci_gate_exit(PropertyVerdict::from(verdict).as_str(), args.ci.fail_on);
    Ok(())
}

/// `mununu sv verify-recoverability` — lift SV and decide `AG EF good`.
/// mununu#541 — `--json` makes an ERROR machine-readable too.
///
/// The success path already printed JSON; a failure printed prose to stderr, so a subprocess
/// consumer had to parse two formats and choose by exit code. The wrapper is deliberately thin —
/// all the work stays in `sv_verify_recoverability_impl`, so the only thing this adds is the output contract.
fn sv_verify_recoverability(args: SvVerifyRecoverabilityArgs) -> Result<(), String> {
    let as_json = args.json.json;
    let file = args.lift.primary_display();
    match sv_verify_recoverability_impl(args) {
        Ok(()) => Ok(()),
        Err(e) if as_json => exit_with_json_error("sv-verify-recoverability", &file, &e),
        Err(e) => Err(e),
    }
}

fn sv_verify_recoverability_impl(args: SvVerifyRecoverabilityArgs) -> Result<(), String> {
    use mununu_core::adapter::recoverability::{
        parse_config_value_specs, parse_extra_predicate, recoverability_property_str,
    };
    use mununu_core::adapter::sv_verify::{
        sv_verify_recoverability_explained, sv_verify_recoverability_refined_explained,
    };

    let file = args.lift.primary_display();
    let extra = args
        .predicate
        .iter()
        .map(|s| parse_extra_predicate(s))
        .collect::<Result<Vec<_>, _>>()?;
    let config_specs = parse_config_value_specs(&args.partition)?;
    let lift = read_sv_lift(&args.lift)?;

    // mununu#541 — build the TYPED report, not an inline literal. The struct is what `schemars`
    // derives the published schema from, so the document a consumer parses and the schema they
    // validate against cannot drift apart.
    let mut report = mununu_core::api::models::SvVerifyRecoverabilityReport {
        file: file.clone(),
        property: recoverability_property_str(&args.target),
        outcome: String::new(),
        refinement: None,
        engine: None,
        exact_abstention: None,
    };
    let (verdict, attribution) =
        if args.refine || !config_specs.is_empty() || args.discover_assumptions {
            let (verdict, refinement, attribution) = sv_verify_recoverability_refined_explained(
                &lift,
                &args.target,
                &extra,
                &config_specs,
                args.discover_assumptions,
            )?;
            report.refinement = Some(
                serde_json::to_value(&refinement)
                    .map_err(|e| format!("serialize refinement: {e}"))?,
            );
            (verdict, attribution)
        } else {
            let (verdict, attribution) =
                sv_verify_recoverability_explained(&lift, &args.target, &extra)?;
            (verdict, Some(attribution))
        };
    report.outcome = verdict.as_str().to_string();
    if let Some(a) = &attribution {
        report.engine = Some(a.engine.as_str().to_string());
        report.exact_abstention = a.exact_abstention.map(|k| k.label().to_string());
    }

    print_typed_report(&report)?;
    ci_gate_exit(verdict.as_str(), args.ci.fail_on);
    Ok(())
}

/// `mununu sv check-fsm` — lift SV then auto-scan every FSM register for a reachable
/// illegal encoding. SV-direct peer of `btor2 check-fsm`; same JSON + CI exit.
fn sv_check_fsm(args: SvCheckFsmArgs) -> Result<(), String> {
    use mununu_core::adapter::sv_verify::sv_check_fsm as core_sv_check_fsm;
    use mununu_core::verdict::PropertyVerdict;

    let file = args.lift.primary_display();
    let findings = core_sv_check_fsm(&read_sv_lift(&args.lift)?, args.max_width)?;

    let registers: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "register": f.register,
                "kind": f.kind.as_str(),
                "legal_encodings": f.legal_encodings,
                "outcome": f.verdict.as_str(),
                "illegal_encoding_reachable": f.is_finding(),
            })
        })
        .collect();
    let mut summary = serde_json::json!({
        "file": file,
        "fsm_registers_checked": findings.iter().filter(|f| f.is_checked()).count(),
        "illegal_encodings_found": findings.iter().filter(|f| f.is_finding()).count(),
        "registers": registers,
    });
    if args.liveness {
        let terminal = mununu_core::api::models::parse_terminal_states(&args.terminal)?;
        let inv = mununu_core::adapter::sv_verify::sv_check_fsm_liveness(
            &read_sv_lift(&args.lift)?,
            args.max_width,
            &terminal,
        )?;
        summary["liveness"] = liveness_json(&inv);
    }
    print_json_summary(&summary)?;

    let worst = worst_verdict(findings.iter().map(|f| PropertyVerdict::as_str(f.verdict)));
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

/// `mununu sv lint` — lift SV then report the partial-write registers the verifier
/// cannot keep faithfully (monono#partsel). CI-time preflight; changes no verdict.
/// A finding maps to the `violated` CI verdict, so the default `--fail-on violated`
/// fails the build when the lift is unfaithful; `--fail-on none` makes it advisory.
fn sv_lint(args: SvLintArgs) -> Result<(), String> {
    use mununu_core::adapter::sv_verify::sv_lint_registers;

    let file = args.lift.primary_display();
    let findings = sv_lint_registers(&read_sv_lift(&args.lift)?)?;

    let signals: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            let mut o = serde_json::json!({
                "signal": f.signal,
                "kind": f.kind.as_str(),
                "rule": f.rule.as_str(),
            });
            if !f.detail.is_empty() {
                o["detail"] = serde_json::Value::String(f.detail.clone());
            }
            o
        })
        .collect();
    let registers_flagged = findings
        .iter()
        .filter(|f| f.kind == mununu_core::adapter::sv_verify::SvLintSignalKind::Register)
        .count();
    let summary = serde_json::json!({
        "file": file,
        "signals_flagged": findings.len(),
        "registers_flagged": registers_flagged,
        "findings": signals,
    });
    print_json_summary(&summary)?;

    // A finding is the lint's `violated`; a clean run is `holds`. Reuses the shared
    // CI gate so `--fail-on {violated|unknown|none}` behaves like the verify verbs.
    let worst = if findings.is_empty() {
        "holds"
    } else {
        "violated"
    };
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

/// `mununu sv cdc-lint` — classify every clock-domain crossing; the JSON is
/// [`SvCdcLintResponse`](mununu_core::api::models::SvCdcLintResponse), shared with the API. The
/// worst row drives the `--fail-on` gate.
fn sv_cdc_lint(args: SvCdcLintArgs) -> Result<(), String> {
    let as_json = args.json.json;
    let file = args.lift.primary_display();
    let run = || -> Result<(), String> {
        let report = mununu_core::adapter::sv_verify::sv_cdc_lint(&read_sv_lift(&args.lift)?)?;
        let mut view = mununu_core::api::models::SvCdcLintResponse::from(&report);
        view.file = Some(file.clone());
        print_typed_report(&view)?;
        ci_gate_exit(&view.outcome, args.ci.fail_on);
        Ok(())
    };
    match run() {
        Ok(()) => Ok(()),
        Err(e) if as_json => exit_with_json_error("sv-cdc-lint", &file, &e),
        Err(e) => Err(e),
    }
}

/// `mununu sv mutate` — apply a named structural mutation + re-verify, checking the
/// property verdicts flip (property adequacy), or (`--list`) enumerate the targets.
/// A mutation caught by NO property maps to the `violated` CI verdict (a coverage
/// gap), so the default `--fail-on violated` fails the build; `--fail-on none` is
/// advisory. A `--list` run always exits 0.
fn sv_mutate(args: SvMutateArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::kmts_lift::MustEdgeInference;
    use mununu_core::adapter::btor2::mutate::{Mutation, list_targets};
    use mununu_core::adapter::slang::verify_auto::{
        PortfolioMode, VerifyAutoOptions, mutate_and_compare,
    };
    use mununu_core::adapter::yosys::{YosysOptions, sv_to_btor2};

    let file = args.lift.primary_display();
    let lift = read_sv_lift(&args.lift)?;
    let primary_name = lift.primary_name.clone();
    let mut sources: Vec<(String, String)> = vec![(primary_name.clone(), lift.source.clone())];
    sources.extend(lift.additional_sources.iter().cloned());
    let config_values = parse_config_values(&args.pin)?;
    // The overrides go to `mutate_and_compare` as verify-auto's own option (the lift only); the
    // yosys options carry the plain sources.
    let lift_overrides = lift.lift_overrides.clone();
    let yopts = YosysOptions {
        primary_source_path: args.lift.file.as_ref().map(|p| p.display().to_string()),
        ..lift.yosys_options()
    };

    // `--list` — lift once and enumerate the mutation targets (no verification). The discovery
    // lift honours `--lift-override` too (mununu#725): it does not go through `verify_auto`, and
    // the real OpenTitan `prim_assert.sv` dies in yosys (a topological loop) without the shim.
    if args.list {
        let (primary, yopts) = lift
            .staged()
            .map_err(|e| format!("sv mutate --list: {e}"))?;
        let btor2 = sv_to_btor2(&primary, &yopts)
            .map_err(|e| format!("sv mutate --list: {}", e.message))?;
        let t = list_targets(&btor2)?;
        let summary = serde_json::json!({
            "file": file,
            "targets": {
                "stick": t.stick,
                "drop_reset": t.drop_reset,
                "off_by_one": t.off_by_one,
                "invert_cond": t.invert_cond,
            },
        });
        print_json_summary(&summary)?;
        return Ok(());
    }

    let spec = args.mutation.as_deref().ok_or_else(|| {
        "sv mutate: --mutation is required (or use --list to discover targets); \
         expected `stick:<reg>` or `drop-reset:<reg>`"
            .to_string()
    })?;
    let mutation = Mutation::parse(spec)?;

    // mununu#663 — `auto` (the default) resolves per property in verify-auto; an explicit `off`
    // switches the resolution off.
    let must_edge_auto = matches!(args.must_edge_inference, MustEdgeInferenceArg::Auto);
    let must_edge_inference = match args.must_edge_inference {
        MustEdgeInferenceArg::Auto | MustEdgeInferenceArg::Off => MustEdgeInference::Off,
        MustEdgeInferenceArg::SmtPerTarget => MustEdgeInference::SmtPerTarget,
        MustEdgeInferenceArg::SmtPerTargetStandard => MustEdgeInference::SmtPerTargetStandard,
        MustEdgeInferenceArg::SmtHyperMust => MustEdgeInference::SmtHyperMust,
    };
    let opts = VerifyAutoOptions {
        must_edge_inference,
        must_edge_auto,
        gate_reset: !args.no_gate_reset,
        symbolic_engine: args.engine == EngineArg::Symbolic,
        exact_symbolic: args.engine == EngineArg::ExactSymbolic,
        portfolio: match args.engine {
            EngineArg::PortfolioSequential => Some(PortfolioMode::Sequential),
            EngineArg::PortfolioParallel => Some(PortfolioMode::Parallel),
            _ => None,
        },
        config_values,
        lift_overrides,
        faults: mununu_core::adapter::btor2::fault::FaultPosture::parse(
            &args.fault,
            args.fault_model.as_deref(),
            args.fault_warmup,
        )?,
        ..Default::default()
    };

    let report = mutate_and_compare(&sources, &yopts, &opts, mutation)
        .map_err(|e| format!("sv mutate: {}", e.message))?;

    let properties: Vec<serde_json::Value> = report
        .properties
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "baseline": p.baseline,
                "mutant": p.mutant,
                "flipped": p.flipped,
            })
        })
        .collect();
    let summary = serde_json::json!({
        "file": file,
        "mutation": report.mutation,
        "flipped": report.flipped,
        "unflipped": report.unflipped,
        "properties": properties,
    });
    print_json_summary(&summary)?;

    // A mutation caught by at least one property (some flip) is `holds`; a mutation
    // NO property catches is `violated` (a coverage gap the CI gate should flag).
    let worst = if report.flipped > 0 {
        "holds"
    } else {
        "violated"
    };
    ci_gate_exit(worst, args.ci.fail_on);
    Ok(())
}

/// Pretty-print a JSON summary to stdout.
/// mununu#541 — print a SCHEMA-PINNED report.
///
/// Peer of [`print_json_summary`], which takes a `serde_json::Value` and therefore accepts whatever
/// an inline literal happened to build. This one takes a type, so the document a consumer parses is
/// the same shape `schemars` publishes under `docs/api-schemas/` and the drift detector guards.
fn print_typed_report<T: serde::Serialize>(report: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(report).map_err(|e| format!("serialize report: {e}"))?
    );
    Ok(())
}

fn print_json_summary(summary: &serde_json::Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(summary).map_err(|e| format!("serialize summary: {e}"))?
    );
    Ok(())
}

/// Build the skeleton `SvAnnotation` from discovered state cells (the pure,
/// testable core of `sv discover`). Multi-bit cells become `ignored`
/// placeholders carrying a width note (the author edits them to
/// `bounded_counter` / `enum`); 1-bit cells are omitted — the bit-blaster
/// handles them natively, so listing them is noise.
fn build_discover_skeleton(
    module: &str,
    cells: &[mununu_core::adapter::yosys::SvStateCell],
) -> mununu_core::adapter::systemverilog::annotation::SvAnnotation {
    use mununu_core::adapter::systemverilog::annotation::{
        SV_ANNOTATION_SCHEMA, SignalAbstraction, SignalAnnotation, SvAnnotation,
    };
    let signals = cells
        .iter()
        .filter(|c| c.width > 1)
        .map(|c| SignalAnnotation {
            name: c.name.clone(),
            abstraction: SignalAbstraction::Ignored,
            note: Some(format!(
                "width={} — TODO concretize: bounded_counter (bound=K) | enum | keep ignored",
                c.width
            )),
            ..Default::default()
        })
        .collect();
    SvAnnotation {
        schema: Some(SV_ANNOTATION_SCHEMA.to_string()),
        module: module.to_string(),
        signals,
        ..Default::default()
    }
}

/// `mununu sv discover <FILE..>` — sidecar-audit C1.1 / finding E1.
fn sv_discover(args: SvDiscoverArgs) -> Result<(), String> {
    use std::fs;

    let (primary, rest) = args
        .files
        .split_first()
        .ok_or("sv discover: at least one .sv file is required")?;
    let content = fs::read_to_string(primary)
        .map_err(|e| format!("failed to read '{}': {e}", primary.display()))?;
    let mut additional_sources: Vec<(String, String)> = Vec::new();
    for src in rest {
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("invalid additional source path: {}", src.display()))?;
        let body = fs::read_to_string(src)
            .map_err(|e| format!("failed to read '{}': {e}", src.display()))?;
        additional_sources.push((name.to_string(), body));
    }

    let yopts = mununu_core::adapter::yosys::YosysOptions {
        top: args.top.clone(),
        additional_sources,
        primary_source_path: Some(primary.display().to_string()),
        use_sv2v: args.frontend.sv2v(),
        frontend: args.frontend.into(),
        ..Default::default()
    };

    let cells = mununu_core::adapter::yosys::sv_discover_state_cells(&content, &yopts)
        .map_err(|e| format!("sv discover: {e}"))?;

    let module = args.top.clone().unwrap_or_else(|| "TOP_MODULE".to_string());
    let skeleton = build_discover_skeleton(&module, &cells);
    let json = serde_json::to_string_pretty(&skeleton)
        .map_err(|e| format!("sv discover: failed to serialize skeleton: {e}"))?;

    // Human summary → stderr; clean skeleton JSON → stdout (or --output).
    let total_bits: u32 = cells.iter().map(|c| c.width).sum();
    let multi_bit = cells.iter().filter(|c| c.width > 1).count();
    eprintln!(
        "sv discover: {} state cell(s), {} state bit(s); {} multi-bit cell(s) in the skeleton, \
         {} 1-bit cell(s) omitted (handled natively).",
        cells.len(),
        total_bits,
        multi_bit,
        cells.len() - multi_bit,
    );
    if total_bits > 20 {
        eprintln!(
            "sv discover: raw state width {total_bits} exceeds MAX_STATE_BITS=20 — \
             concretize the multi-bit cells above (e.g. bounded_counter) to fit the bit-blaster."
        );
    }

    match &args.output {
        Some(path) => {
            fs::write(path, format!("{json}\n"))
                .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
            eprintln!("sv discover: wrote skeleton sidecar to {}", path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}

/// `mununu sv validate <SIDECAR>` — sidecar-audit C0.2.
///
/// Surfaces the shared C0.1 load-time lint
/// ([`mununu_core::adapter::systemverilog::annotation::lint_annotation_json`])
/// as a standalone check: hard-fails on a removed `$schema`, reports unknown
/// fields (typo guard), and confirms the sidecar deserializes. `--strict`
/// turns warnings into a non-zero exit.
fn sv_validate(args: SvValidateArgs) -> Result<(), String> {
    use mununu_core::adapter::systemverilog::annotation;

    let label = args.sidecar.display().to_string();
    let content = std::fs::read_to_string(&args.sidecar)
        .map_err(|e| format!("failed to read sidecar '{label}': {e}"))?;

    // Lint first — this is the hard-fail (removed `$schema`) + warnings path.
    let warnings = annotation::lint_annotation_json(&content, &label)?;

    // Then confirm it actually deserializes into the sidecar model (catches
    // type errors the key-level lint does not, e.g. a string where an int
    // is expected).
    serde_json::from_str::<annotation::SvAnnotation>(&content)
        .map_err(|e| format!("sidecar '{label}': failed to parse — {e}"))?;

    if warnings.is_empty() {
        println!("OK: sidecar '{label}' is valid (no warnings).");
        return Ok(());
    }

    eprintln!("{} warning(s) for sidecar '{label}':", warnings.len());
    for w in &warnings {
        eprintln!("  - {w}");
    }
    if args.strict {
        return Err(format!(
            "{} warning(s) found and --strict was set",
            warnings.len()
        ));
    }
    println!(
        "sidecar '{label}' loaded with {} warning(s) (re-run with --strict to fail on them).",
        warnings.len()
    );
    Ok(())
}

fn sv_emit_btor2_per_module(args: SvEmitBtor2PerModuleArgs) -> Result<(), String> {
    use std::collections::HashMap;
    use std::fs;

    let primary_content = fs::read_to_string(&args.file).map_err(|e| {
        format!(
            "Failed to read primary source '{}': {e}",
            args.file.display()
        )
    })?;
    let mut additional: HashMap<String, String> = HashMap::new();
    for src in &args.sources {
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Invalid additional source path: {}", src.display()))?;
        let body = fs::read_to_string(src)
            .map_err(|e| format!("Failed to read additional source '{}': {e}", src.display()))?;
        additional.insert(name.to_string(), body);
    }
    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        args.file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    });

    let yopts = mununu_core::adapter::yosys::YosysOptions {
        top: args.top.clone(),
        additional_sources: additional.into_iter().collect(),
        primary_source_path: Some(args.file.display().to_string()),
        use_sv2v: args.frontend.sv2v(),
        frontend: args.frontend.into(),
        setundef_anyseq: args.setundef_anyseq,
        setundef_anyconst: args.setundef_anyconst,
        per_module_btor: true,
        per_module_output_dir: Some(output_dir.clone()),
        ..Default::default()
    };
    let opts = mununu_core::adapter::AdapterOptions::default();
    let outputs =
        mununu_core::adapter::yosys::translate_sv_per_module(&primary_content, &opts, &yopts)
            .map_err(|e| format!("sv emit-btor2-per-module: {e}"))?;

    println!(
        "Emitted {} BTOR2 file(s) to {}",
        outputs.len(),
        output_dir.display()
    );
    for per_module in &outputs {
        let path_display = per_module
            .btor2_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(transient)".to_string());
        println!(
            "  {} -> {} (state_count={}, property_count={})",
            per_module.module_name,
            path_display,
            per_module.output.source_info.state_count,
            per_module.output.source_info.property_count,
        );
    }
    Ok(())
}

fn sv_preprocess(args: SvPreprocessArgs) -> Result<(), String> {
    if args.files.is_empty() {
        return Err("sv preprocess: at least one .sv input file required".to_string());
    }
    let output_path = args.output.unwrap_or_else(|| {
        let first = &args.files[0];
        let stem = first
            .file_stem()
            .unwrap_or_default()
            .to_str()
            .unwrap_or("module");
        first.with_file_name(format!("{stem}.elab.v"))
    });
    let sv2v =
        mununu_core::adapter::yosys::preprocess_sv(&args.files, &args.include_dirs, &output_path)
            .map_err(|e| format!("sv preprocess: {e}"))?;
    println!(
        "sv2v ({}) -> {} (inputs: {})",
        sv2v.display(),
        output_path.display(),
        args.files.len()
    );
    Ok(())
}

fn extraction_validate(args: ExtractionValidateArgs) -> Result<(), String> {
    let spec_content = fs::read_to_string(&args.spec)
        .map_err(|e| format!("Failed to read spec '{}': {e}", args.spec.display()))?;

    let report = mununu_core::adapter::extraction::validate::validate_spec(
        &spec_content,
        &args.source,
        args.drift_window,
    )?;

    if args.json {
        // JSON output
        let anchors: Vec<serde_json::Value> = report
            .anchors
            .iter()
            .map(|a| {
                use mununu_core::adapter::extraction::validate::AnchorResult;
                match a {
                    AnchorResult::Exact {
                        spec_id,
                        section,
                        line,
                    } => serde_json::json!({
                        "status": "exact", "spec_id": spec_id, "section": section, "line": line
                    }),
                    AnchorResult::Drifted {
                        spec_id,
                        section,
                        expected_line,
                        found_line,
                        drift,
                    } => serde_json::json!({
                        "status": "drifted", "spec_id": spec_id, "section": section,
                        "expected_line": expected_line, "found_line": found_line, "drift": drift
                    }),
                    AnchorResult::Mismatch {
                        spec_id,
                        section,
                        expected_line,
                        expected_pattern,
                        actual_at_line,
                    } => serde_json::json!({
                        "status": "mismatch", "spec_id": spec_id, "section": section,
                        "expected_line": expected_line, "expected_pattern": expected_pattern,
                        "actual_at_line": actual_at_line
                    }),
                    AnchorResult::Error {
                        spec_id,
                        section,
                        message,
                    } => serde_json::json!({
                        "status": "error", "spec_id": spec_id, "section": section,
                        "message": message
                    }),
                }
            })
            .collect();

        let output = serde_json::json!({
            "summary": {
                "total": report.summary.total,
                "exact": report.summary.exact,
                "drifted": report.summary.drifted,
                "mismatch": report.summary.mismatch,
                "error": report.summary.error,
                "uncovered_accesses": report.summary.uncovered_accesses,
            },
            "commit_match": report.commit_match,
            "anchors": anchors,
        });
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        // Human-readable output
        if let Some(matches) = report.commit_match {
            println!("Commit: {}", if matches { "MATCH" } else { "MISMATCH" });
            println!();
        }

        println!(
            "Anchor checks: {} total, {} exact, {} drifted, {} MISMATCH, {} ERROR",
            report.summary.total,
            report.summary.exact,
            report.summary.drifted,
            report.summary.mismatch,
            report.summary.error,
        );
        println!();

        for a in &report.anchors {
            use mununu_core::adapter::extraction::validate::AnchorResult;
            match a {
                AnchorResult::Exact { spec_id, .. } => {
                    println!("  OK    {spec_id}");
                }
                AnchorResult::Drifted {
                    spec_id,
                    expected_line,
                    found_line,
                    drift,
                    ..
                } => {
                    println!(
                        "  DRIFT {spec_id} (expected line {expected_line}, found at {found_line}, drift={drift:+})"
                    );
                }
                AnchorResult::Mismatch {
                    spec_id,
                    expected_line,
                    expected_pattern,
                    actual_at_line,
                    ..
                } => {
                    println!("  FAIL  {spec_id} (line {expected_line})");
                    println!("         expected: {expected_pattern}");
                    println!("         actual:   {actual_at_line}");
                }
                AnchorResult::Error {
                    spec_id, message, ..
                } => {
                    println!("  ERR   {spec_id}: {message}");
                }
            }
        }

        if !report.uncovered.is_empty() {
            println!(
                "\nUncovered state field accesses: {}",
                report.uncovered.len()
            );
            for u in report.uncovered.iter().take(20) {
                println!("  line {:4}: {:20}  {}", u.line, u.field, u.content);
            }
            if report.uncovered.len() > 20 {
                println!("  ... and {} more", report.uncovered.len() - 20);
            }
        }
    }

    if report.summary.mismatch > 0 || report.summary.error > 0 {
        Err(format!(
            "{} mismatch(es) and {} error(s) found",
            report.summary.mismatch, report.summary.error
        ))
    } else {
        Ok(())
    }
}

fn extraction_check(args: ExtractionCheckArgs) -> Result<(), String> {
    let mut failures = 0;

    for path in &args.files {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;

        let info = mununu_core::adapter::extraction::validate::check_provenance(&content);
        let name = path.file_name().unwrap_or_default().to_string_lossy();

        if args.require_generated && !info.is_generated() {
            println!("FAIL: {name} — missing @generated-from header");
            failures += 1;
        } else if args.require_model_source && !info.is_specification_model() {
            println!("FAIL: {name} — missing @model-source header");
            failures += 1;
        } else if info.is_generated() {
            println!(
                "OK:   {name} — @generated-from: {}",
                info.generated_from.as_deref().unwrap_or("?")
            );
        } else if info.is_specification_model() {
            println!(
                "OK:   {name} — @model-source: {}",
                info.model_source.as_deref().unwrap_or("?")
            );
        } else {
            println!("WARN: {name} — no provenance header");
        }
    }

    if failures > 0 {
        Err(format!("{failures} file(s) missing required headers"))
    } else {
        Ok(())
    }
}

fn prepare_output_dir(path: &Path, force: bool) -> Result<(), String> {
    if path.exists() {
        if !path.is_dir() {
            return Err(format!(
                "output path '{}' exists but is not a directory",
                path.display()
            ));
        }
        if force {
            if let Ok(mut entries) = fs::read_dir(path)
                && entries.next().is_some()
            {
                tracing::warn!(
                    path = %path.display(),
                    "output directory not empty; existing files may be overwritten"
                );
            }
        } else {
            let mut entries = fs::read_dir(path)
                .map_err(|err| format!("failed to inspect '{}': {err}", path.display()))?;
            if entries.next().is_some() {
                return Err(format!(
                    "output directory '{}' is not empty (use --force to overwrite)",
                    path.display()
                ));
            }
        }
    } else {
        fs::create_dir_all(path).map_err(|err| {
            format!(
                "failed to create output directory '{}': {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Default)]
struct ModalityBreakdown {
    sharp: usize,
    may_only: usize,
    must_hyper_only: usize,
}

#[derive(Debug, Serialize)]
struct AutomatonSummaryOutput {
    name: String,
    state_count: usize,
    transition_count: usize,
    /// R.5 Item K sub-item K.4 (2026-06-05) — per-automaton KMTS
    /// modality breakdown sourced from the CTXDSL AST. Sharp is the
    /// pre-K.4 default; non-zero `may_only` / `must_hyper_only`
    /// indicates the automaton declares KMTS-shaped transitions via
    /// the K.1 attribute syntax. Serialized verbatim into the
    /// `context summarize --format json` output.
    #[serde(skip_serializing_if = "modality_breakdown_is_sharp_only")]
    modality_breakdown: ModalityBreakdown,
}

/// R.5 Item K sub-item K.4 — serde skip predicate. Keeps pre-K.4
/// JSON byte-for-byte compatible: when an automaton only carries
/// Sharp transitions (the dominant case), the `modality_breakdown`
/// field is omitted entirely.
fn modality_breakdown_is_sharp_only(b: &ModalityBreakdown) -> bool {
    b.may_only == 0 && b.must_hyper_only == 0
}

#[derive(Debug, Serialize)]
struct ContextSummaryOutput {
    context: String,
    sidecar_count: usize,
    automata: Vec<AutomatonSummaryOutput>,
    controllers: Vec<String>,
    formulas: Vec<String>,
    guard_predicates: BTreeMap<String, Vec<String>>,
}

fn build_context_summary(
    context_doc: &ContextDoc,
    sidecar_docs: &[ContextDoc],
    realized: &RealizedContext,
) -> ContextSummaryOutput {
    let mut automata_map: BTreeMap<String, AutomatonSummaryOutput> = BTreeMap::new();
    for doc in std::iter::once(context_doc).chain(sidecar_docs.iter()) {
        for automaton in &doc.automata {
            let mut breakdown = ModalityBreakdown::default();
            for t in &automaton.transitions {
                match t.modality {
                    TransitionModalitySpec::Sharp => breakdown.sharp += 1,
                    TransitionModalitySpec::MayOnly => breakdown.may_only += 1,
                    TransitionModalitySpec::MustOnly => breakdown.must_hyper_only += 1,
                }
            }
            automata_map
                .entry(automaton.name.name.clone())
                .or_insert_with(|| AutomatonSummaryOutput {
                    name: automaton.name.name.clone(),
                    state_count: automaton.states.len(),
                    transition_count: automaton.transitions.len(),
                    modality_breakdown: breakdown,
                });
        }
    }

    let automata = automata_map.into_values().collect();

    let mut controllers = BTreeSet::new();
    let mut formulas = BTreeSet::new();
    for doc in std::iter::once(context_doc).chain(sidecar_docs.iter()) {
        for controller in &doc.controllers {
            controllers.insert(controller.name.name.clone());
        }
        for formula in &doc.mu_formulas {
            formulas.insert(formula.name.name.clone());
        }
    }

    let mut guard_predicates = BTreeMap::new();
    for (automaton, predicates) in realized.predicates.iter() {
        let mut list: Vec<String> = predicates.iter().cloned().collect();
        list.sort();
        guard_predicates.insert(automaton.clone(), list);
    }

    ContextSummaryOutput {
        context: context_doc.name.name.clone(),
        sidecar_count: sidecar_docs.len(),
        automata,
        controllers: controllers.into_iter().collect(),
        formulas: formulas.into_iter().collect(),
        guard_predicates,
    }
}

fn context_merge(args: ContextMergeArgs) -> Result<(), String> {
    if args.files.is_empty() {
        return Err("at least one context file must be provided".into());
    }

    let context_path = args.files[0].clone();
    let sidecar_paths: Vec<PathBuf> = args.files.iter().skip(1).cloned().collect();
    let (context_doc, sidecar_docs, _) =
        load_context_documents(&context_path, &sidecar_paths, None)?;
    let realized = realize_documents(&context_doc, &sidecar_docs)?;
    let summary = build_context_summary(&context_doc, &sidecar_docs, &realized);

    println!("Merged context '{}'", summary.context);
    println!("  Sidecars: {}", summary.sidecar_count);
    println!("  Automata:");
    if summary.automata.is_empty() {
        println!("    (none)");
    } else {
        for automaton in &summary.automata {
            let breakdown = &automaton.modality_breakdown;
            let modality_suffix = if modality_breakdown_is_sharp_only(breakdown) {
                String::new()
            } else {
                // R.5 Item K sub-item K.4 (2026-06-05) — surface KMTS
                // modality breakdown when the automaton declares non-Sharp
                // transitions via the K.1 attribute syntax. Pre-K.4
                // automata (Sharp-only) print the original output
                // byte-for-byte.
                format!(
                    " [modality: sharp={}, may_only={}, must_hyper_only={}]",
                    breakdown.sharp, breakdown.may_only, breakdown.must_hyper_only
                )
            };
            println!(
                "    - {} (states: {}, transitions: {}){}",
                automaton.name, automaton.state_count, automaton.transition_count, modality_suffix
            );
        }
    }
    if summary.formulas.is_empty() {
        println!("  μ-formulas: none");
    } else {
        println!("  μ-formulas: {}", summary.formulas.join(", "));
    }
    if summary.controllers.is_empty() {
        println!("  Controllers: none");
    } else {
        println!("  Controllers: {}", summary.controllers.join(", "));
    }

    if summary.guard_predicates.is_empty() {
        println!("  Guard predicates: none");
    } else {
        println!("  Guard predicates:");
        for (automaton, predicates) in &summary.guard_predicates {
            if predicates.is_empty() {
                println!("    - {}: none", automaton);
            } else {
                println!("    - {}: {}", automaton, predicates.join(", "));
            }
        }
    }

    if let Some(output_dir) = args.output {
        prepare_output_dir(&output_dir, args.force)?;
        let mut copied = 0usize;
        for input in std::iter::once(&context_path).chain(sidecar_paths.iter()) {
            let Some(filename) = input.file_name() else {
                return Err(format!(
                    "cannot determine file name for '{}'",
                    input.display()
                ));
            };
            let destination = output_dir.join(filename);
            fs::copy(input, &destination).map_err(|err| {
                format!(
                    "failed to copy '{}' to '{}': {err}",
                    input.display(),
                    destination.display()
                )
            })?;
            copied += 1;
        }
        println!("Copied {} file(s) to {}", copied, output_dir.display());
    }

    Ok(())
}

fn context_summarize(args: ContextSummarizeArgs) -> Result<(), String> {
    let preprocessor = validate_preprocessor(args.preprocessor.as_deref())?;
    let (context_doc, sidecar_docs, _) = load_context_documents_mode(
        &args.context,
        &args.sidecars,
        args.adapter.as_deref(),
        args.mode.as_deref(),
        preprocessor,
    )?;
    let realized = realize_documents(&context_doc, &sidecar_docs)?;
    let summary = build_context_summary(&context_doc, &sidecar_docs, &realized);
    let json = serde_json::to_string_pretty(&summary)
        .map_err(|err| format!("failed to serialise summary: {err}"))?;
    println!("{json}");

    // Print structure if requested
    if let Some(output_path) = args.print_structure {
        print_context_structure(&realized.context, output_path)?;
    }

    Ok(())
}

fn context_predicates(args: ContextPredicatesArgs) -> Result<(), String> {
    let preprocessor = validate_preprocessor(args.preprocessor.as_deref())?;
    let (context_doc, sidecar_docs, _) = load_context_documents_mode(
        &args.context,
        &args.sidecars,
        args.adapter.as_deref(),
        args.mode.as_deref(),
        preprocessor,
    )?;
    let realized = realize_documents(&context_doc, &sidecar_docs)?;
    let mut automata: Vec<String> = realized.predicates.keys().cloned().collect();
    automata.sort();

    let filter = args.automaton.as_deref();
    let mut reported = false;

    for automaton in automata {
        if let Some(filter) = filter
            && automaton != filter
        {
            continue;
        }

        let Some(predicates) = realized.predicate_names(&automaton) else {
            continue;
        };

        let mut names: Vec<String> = predicates.iter().cloned().collect();
        names.sort();

        println!("Automaton: {}", automaton);
        if names.is_empty() {
            println!("  (no guard predicates)");
        } else {
            for predicate in names {
                let guard = realized
                    .predicate_formula(&automaton, &predicate)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if guard.is_empty() {
                    println!("  - {}", predicate);
                } else {
                    println!("  - {} => {}", predicate, guard);
                }
            }
        }
        reported = true;
    }

    if let Some(filter_name) = filter
        && !reported
    {
        return Err(format!(
            "automaton '{}' has no registered predicates",
            filter_name
        ));
    }

    if !reported {
        println!("No guard predicates registered.");
    }

    Ok(())
}

fn context_eval(args: ContextEvalArgs) -> Result<(), String> {
    let as_json = args.json.json;
    let file = args.context.display().to_string();
    match context_eval_impl(args) {
        Ok(()) => Ok(()),
        Err(e) if as_json => exit_with_json_error("context-eval", &file, &e),
        Err(e) => Err(e),
    }
}

/// Print `context eval`'s JSON document and apply the CI gate.
fn print_context_eval_json(
    results: Vec<mununu_core::api::models::FormulaVerificationResult>,
    sanity: Vec<mununu_core::context::sanity::SanityReport>,
    fail_on: FailOn,
) -> Result<(), String> {
    let holds = results.iter().all(|r| r.outcome == "holds");
    let formula_holds = (!results.is_empty()).then_some(holds);
    let outcome = context_eval_outcome(formula_holds, sanity.iter().all(|r| r.is_clean()));
    let doc = serde_json::json!({
        "success": true,
        "outcome": outcome,
        "results": results,
        "sanity": sanity,
    });
    print_json_summary(&doc)?;
    ci_gate_exit(outcome, fail_on);
    Ok(())
}

fn context_eval_impl(args: ContextEvalArgs) -> Result<(), String> {
    let as_json = args.json.json;
    let mut sanity_reports = Vec::new();
    let preprocessor = validate_preprocessor(args.preprocessor.as_deref())?;
    let PreparedEvalContext {
        realized,
        formula_name,
    } = prepare_eval_context(EvalContextParams {
        context: &args.context,
        sidecars: &args.sidecars,
        adapter: args.adapter.as_deref(),
        mode: args.mode.as_deref(),
        preprocessor,
        print_ctxdsl_path: args.print_ctxdsl.as_ref(),
        stubs: &args.stubs,
        formula: &args.formula,
        template: &args.template,
        template_args: &args.template_args,
        templates_files: &args.templates_files,
        automaton: &args.automaton,
        allow_no_formula: args.sanity,
    })?;
    let mut sanity_clean = true;
    if args.sanity {
        let clts = realized
            .context
            .clts(&args.automaton)
            .ok_or_else(|| format!("unknown automaton '{}' in realised context", args.automaton))?;
        let members: Vec<(String, &mununu_core::clts::Clts<_, _>)> = realized
            .members_of(&args.automaton)
            .unwrap_or(&[])
            .iter()
            .filter_map(|m| realized.context.clts(m).map(|c| (m.clone(), c)))
            .collect();
        let report = mununu_core::context::sanity::check(
            &args.automaton,
            clts,
            &realized.declared_labels_of(&args.automaton),
            &members,
        );
        sanity_clean = report.is_clean();
        if !as_json {
            print!("{report}");
        }
        sanity_reports.push(report);
        if formula_name.is_empty() {
            if as_json {
                return print_context_eval_json(Vec::new(), sanity_reports, args.ci.fail_on);
            }
            ci_gate_exit(context_eval_outcome(None, sanity_clean), args.ci.fail_on);
            return Ok(());
        }
    }
    let formula = realized
        .formulas
        .get(&formula_name)
        .ok_or_else(|| format!("unknown formula '{}' in realised context", formula_name))?;
    let clts = realized
        .context
        .clts(&args.automaton)
        .ok_or_else(|| format!("unknown automaton '{}' in realised context", args.automaton))?;

    // Apply adaptation: hiding + minimization (before evaluation)
    // If adaptation is applied, evaluate on the adapted CLTS directly.
    let adapted_clts: Option<
        mununu_core::clts::Clts<
            mununu_core::clts::DefaultStateIdx,
            mununu_core::clts::DefaultLabelIdx,
        >,
    > = {
        let need_adaptation = !args.hide.is_empty() || args.minimize;
        if need_adaptation {
            let mut working = if !args.hide.is_empty() {
                let hide_set: std::collections::HashSet<String> =
                    args.hide.iter().cloned().collect();
                let (hidden, stats) =
                    mununu_core::composition::hide::hide_labels_with_stats(clts, &hide_set)
                        .map_err(|e| format!("label hiding failed: {e}"))?;
                eprintln!(
                    "Hidden {} label(s) out of {} total",
                    stats.labels_hidden, stats.total_labels
                );
                hidden
            } else {
                // Clone the original CLTS for minimization
                // (minimization needs ownership; we can't modify the realized context)
                // Use hide with empty set as identity clone
                mununu_core::composition::hide::hide_labels(clts, &std::collections::HashSet::new())
                    .map_err(|e| format!("CLTS copy failed: {e}"))?
            };

            if args.minimize {
                match mununu_core::composition::minimize::minimize_bisimulation(&working, None)
                    .map_err(|e| format!("minimization failed: {e}"))?
                {
                    Some((minimized, report)) => {
                        eprintln!(
                            "Minimized: {} → {} states ({} removed), {} → {} transitions",
                            report.states_before,
                            report.states_after,
                            report.states_before - report.states_after,
                            report.transitions_before,
                            report.transitions_after,
                        );
                        working = minimized;
                    }
                    None => {
                        eprintln!("Minimization: already minimal (no reduction)");
                    }
                }
            }

            Some(working)
        } else {
            None
        }
    };

    // Choose which CLTS to evaluate on
    let eval_clts = adapted_clts.as_ref().unwrap_or(clts);

    // Build environment matching the CLTS state count.
    // For adapted CLTSs, register state-name predicates so formulas
    // referencing state names (e.g., `!Closed`) resolve correctly.
    let env = if adapted_clts.is_some() {
        let sc = eval_clts.state_count();
        let mut env = mununu_core::mu_calculus::Environment::new(sc);
        for state_id in eval_clts.states() {
            if let Some(name) = eval_clts.state_name(state_id) {
                let mut bits = bitvec::vec::BitVec::<usize, bitvec::order::Lsb0>::repeat(false, sc);
                bits.set(state_id.index(), true);
                env = env.with_predicate(name.to_string(), bits);
            }
        }
        env
    } else {
        realized.environment_for(&args.automaton)
    };

    let mut options = EvaluationOptions::default();
    if args.no_partitions {
        options.use_partitions = false;
    }

    let result = mununu_core::mu_calculus::evaluate_with_options(
        &formula.formula,
        eval_clts,
        &env,
        &options,
    )
    .map_err(|err| format!("μ-calculus evaluation failed: {err}"))?;

    let mut satisfying = Vec::new();
    for state_id in eval_clts.states() {
        if result
            .get(state_id.index())
            .map(|bit| *bit)
            .unwrap_or(false)
            && let Some(name) = eval_clts.state_name(state_id)
        {
            satisfying.push(name.to_string());
        }
    }
    satisfying.sort();

    let initial_states: Vec<String> = eval_clts
        .initial_states()
        .iter()
        .filter_map(|state_id| eval_clts.state_name(*state_id).map(|name| name.to_string()))
        .collect();

    let mut initial_satisfying: Vec<String> = eval_clts
        .initial_states()
        .iter()
        .filter_map(|state_id| {
            if result
                .get(state_id.index())
                .map(|bit| *bit)
                .unwrap_or(false)
            {
                eval_clts.state_name(*state_id).map(|name| name.to_string())
            } else {
                None
            }
        })
        .collect();
    initial_satisfying.sort();

    // Counterexample-style output: when some initial states violate
    // the property, surface them by name AND by structured valuation
    // (`signal = value` pairs from the BTOR2 lifter's cross-product
    // enumeration). Lets the engineer read "init state s_init_5 has
    // boot_fsm_ns = UNMATCHED_5" directly from the verdict instead of
    // mentally decoding state IDs against the cell domains. Empty
    // when every initial state satisfies, or when no state carries a
    // structured valuation (most CTXDSL-only fixtures).
    let initial_violating: Vec<(String, Option<BTreeMap<String, String>>)> = eval_clts
        .initial_states()
        .iter()
        .filter_map(|state_id| {
            let satisfies = result
                .get(state_id.index())
                .map(|bit| *bit)
                .unwrap_or(false);
            if satisfies {
                return None;
            }
            let name = eval_clts.state_name(*state_id)?.to_string();
            let valuation = eval_clts.state_valuation(*state_id).cloned();
            Some((name, valuation))
        })
        .collect();

    // GAP-009: vacuous-property warning. Surfaced by the 2026-05-04
    // compositional validations on MCP-001 / MCP-005, where per-instance
    // automata collapsed to 1 state and verdicts numerically matched the
    // hand baselines by coincidence. The check + message are extracted
    // into `vacuity_warning_for_state_count` for unit testability;
    // surfaced via stderr alongside the existing soundness warnings.
    if let Some(msg) = vacuity_warning_for_state_count(eval_clts.state_count()) {
        eprintln!("{msg}");
    }
    if as_json {
        let result = mununu_core::api::models::FormulaVerificationResult {
            formula_name: formula_name.clone(),
            automaton: args.automaton.clone(),
            outcome: if initial_satisfying.len() == initial_states.len() {
                "holds"
            } else {
                "violated"
            }
            .to_string(),
            total_states: eval_clts.state_count(),
            satisfying_states: satisfying.len(),
            initial_states: initial_states.clone(),
            initial_satisfying: initial_satisfying.clone(),
            initial_violating: initial_violating.iter().map(|(n, _)| n.clone()).collect(),
            satisfying_state_names: satisfying.clone(),
            counterstrategy: None,
        };
        return print_context_eval_json(vec![result], sanity_reports, args.ci.fail_on);
    }

    println!(
        "Formula '{}' over automaton '{}':",
        formula_name, args.automaton
    );
    println!(
        "  States satisfying: {}/{}",
        satisfying.len(),
        eval_clts.state_count()
    );
    if satisfying.is_empty() {
        println!("    (none)");
    } else {
        println!("    {}", satisfying.join(", "));
    }
    println!(
        "  Initial states satisfying: {}/{}",
        initial_satisfying.len(),
        initial_states.len()
    );
    if initial_satisfying.is_empty() {
        println!("    (none)");
    } else {
        println!("    {}", initial_satisfying.join(", "));
    }

    // Counterexample-style output: surface violating initial states
    // with their structured valuations (when the adapter produced
    // them). For Caliptra-shape fixtures this prints lines like
    // `s_init_5  boot_fsm_ns = UNMATCHED_5, wait_count = ZERO` which
    // directly identifies the bug-bearing reset samples — closes the
    // diagnosability gap the §6.7 measurement-driven discipline + the
    // Substack-article roadmap section both flagged.
    if !initial_violating.is_empty() {
        println!(
            "  Initial states violating: {}/{}",
            initial_violating.len(),
            initial_states.len()
        );
        for (name, valuation) in &initial_violating {
            match valuation {
                Some(vals) if !vals.is_empty() => {
                    let pairs: Vec<String> =
                        vals.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                    println!("    {name}  ({})", pairs.join(", "));
                }
                _ => println!("    {name}"),
            }
        }
    }
    println!(
        "  Guard partitions: {}",
        if options.use_partitions {
            "enabled"
        } else {
            "disabled"
        }
    );

    // Soundness trust-level warning for liveness on over-approximate models
    if formula.alternation_depth >= 2 {
        let has_noop = eval_clts.states().any(|state| {
            eval_clts.outgoing(state).iter().any(|t| {
                t.target() == state
                    && t.labels().iter().any(|l| {
                        eval_clts
                            .label_payload(*l)
                            .is_some_and(|syms| syms.iter().any(|s| s == "noop" || s == "tau"))
                    })
            })
        });
        if has_noop {
            eprintln!(
                "  [SOUNDNESS WARNING] Trust level: LOW — formula has alternation depth {} \
                 (class: {:?}) and model contains noop/tau self-loops. Liveness verdicts \
                 may not transfer to the real system (over-approximation admits spurious progress).",
                formula.alternation_depth, formula.property_class
            );
        } else {
            eprintln!(
                "  [SOUNDNESS NOTE] Formula has alternation depth {} (class: {:?}). \
                 Positional strategy extraction is best-effort for this class.",
                formula.alternation_depth, formula.property_class
            );
        }
    }

    if args.soundness_report {
        print_soundness_report(&formula_name, formula, eval_clts);
    }

    // Print structure if requested
    if let Some(output_path) = args.print_structure {
        print_context_structure(&realized.context, output_path)?;
    }

    let holds = initial_satisfying.len() == initial_states.len();
    ci_gate_exit(
        context_eval_outcome(Some(holds), sanity_clean),
        args.ci.fail_on,
    );
    Ok(())
}

use mununu_core::api::models::context_outcome as context_eval_outcome;

/// GR(1) controller synthesis path (`--controller-mode gr1`): read the source,
/// translate it to the adapter IR, run the sound GR(1) synthesizer, print the
/// verdict, and optionally write the controller SystemVerilog (`--emit-sv`).
/// Currently supports TLSF sources (LTL assume/guarantee specs).
/// Flags the `--controller-mode gr1` path cannot honour, each with what to do instead.
///
/// mununu#541 — the gr1 path reads `context`, `adapter` and `emit_sv`, and previously IGNORED every
/// other flag of its own subcommand **silently**, exiting 0. rosf measured that as
/// `--dump-json <FILE>` accepted, exit 0, and no file written. That is the accepted-and-ignored class
/// mununu#499 and rosf#37 both landed on.
///
/// This table is the guard, and it is the durable half of the fix: adding a flag to
/// `ContextSynthesizeArgs` without deciding whether gr1 honours it now fails
/// `every_context_synth_flag_is_honoured_or_refused_on_the_gr1_path`, rather than silently joining
/// the ignored set. A refusal is a better answer than a no-op, because a no-op that exits 0 is
/// indistinguishable from success.
const GR1_UNSUPPORTED_FLAGS: &[(&str, &str)] = &[
    (
        "--sidecar",
        "gr1 synthesises from the TLSF spec's own assumptions and guarantees; there is no CLTS for a sidecar to annotate",
    ),
    (
        "--mode",
        "adapter mode selection applies to the projection path's realize step",
    ),
    ("--preprocessor", "the TLSF path has no preprocessor stage"),
    (
        "--formula",
        "gr1 takes the STRUCTURED spec (assumptions + guarantees + signal directions), not a single mu-calculus formula",
    ),
    (
        "--template",
        "gr1 takes the structured spec, not a property template; a template names ONE formula to realize",
    ),
    (
        "--template-arg",
        "only meaningful with --template, which this path does not accept",
    ),
    (
        "--no-partitions",
        "partition refinement belongs to the projection path",
    ),
    (
        "--minimize",
        "no structural minimisation pass is implemented for the emitted Mealy module",
    ),
    (
        "--counterexample",
        "gr1 reports realizability; an unrealizability WITNESS is `btor2 game --objective recurrence`'s stall lasso, not this verb",
    ),
    (
        "--deadlock-traces",
        "the game has no deadlock notion — an env-blocked state is unrealizability",
    ),
    (
        "--max-counter-traces",
        "bounds a counterexample-trace count this path never produces; unrealizability is a verdict here, not a trace set",
    ),
    (
        "--no-proof-obligations",
        "proof obligations are emitted by the projection path",
    ),
    (
        "--emit-dsl",
        "gr1 emits a SystemVerilog Mealy controller, not CTXDSL; use --emit-sv",
    ),
    (
        "--dump-diagnostics",
        "the projection path's diagnostics record; gr1's are in --dump-json",
    ),
    ("--print-structure", "projection-path structure dump"),
    (
        "--print-ctxdsl",
        "gr1 emits a SystemVerilog Mealy controller; there is no CTXDSL rendering of a game strategy to print",
    ),
    (
        "--output-format",
        "gr1's machine-readable output is --dump-json",
    ),
    ("--emit-native", "projection-path artifact"),
    (
        "--soundness-report",
        "the projection path's abstraction-soundness record; gr1's soundness is the Piterman fixpoint itself",
    ),
];

/// Which of [`GR1_UNSUPPORTED_FLAGS`] the user actually passed.
///
/// Only NON-DEFAULT values count, so a user who never mentions a flag is never told about it.
fn gr1_rejected_flags(args: &ContextSynthesizeArgs) -> Vec<&'static (&'static str, &'static str)> {
    let set: &[(&str, bool)] = &[
        ("--sidecar", !args.sidecars.is_empty()),
        ("--mode", args.mode.is_some()),
        ("--preprocessor", args.preprocessor.is_some()),
        ("--formula", args.formula.is_some()),
        ("--template", args.template.is_some()),
        ("--template-arg", !args.template_args.is_empty()),
        ("--no-partitions", args.no_partitions),
        ("--minimize", args.minimize),
        ("--counterexample", args.counterexample),
        ("--deadlock-traces", args.deadlock_traces),
        ("--max-counter-traces", args.max_counter_traces.is_some()),
        ("--no-proof-obligations", args.no_proof_obligations),
        ("--emit-dsl", args.emit_dsl.is_some()),
        ("--dump-diagnostics", args.dump_diagnostics.is_some()),
        ("--print-structure", args.print_structure.is_some()),
        ("--print-ctxdsl", args.print_ctxdsl.is_some()),
        ("--output-format", args.output_format.is_some()),
        ("--emit-native", args.emit_native.is_some()),
        ("--soundness-report", args.soundness_report),
    ];
    set.iter()
        .filter(|(_, passed)| *passed)
        .filter_map(|(name, _)| GR1_UNSUPPORTED_FLAGS.iter().find(|(n, _)| n == name))
        .collect()
}

fn context_synthesize_gr1(args: &ContextSynthesizeArgs) -> Result<(), String> {
    // mununu#541 — refuse what this path cannot do, rather than accepting it and exiting 0.
    let rejected = gr1_rejected_flags(args);
    if !rejected.is_empty() {
        let mut msg = String::from(
            "--controller-mode gr1 cannot honour the following flag(s). They were previously \
             accepted and silently ignored, which is why this is now an error:\n",
        );
        for (name, why) in &rejected {
            msg.push_str(&format!("  {name} — {why}\n"));
        }
        return Err(msg);
    }
    // `--automaton` is the one exception, and it is structural rather than a decision: clap makes it
    // REQUIRED for this subcommand, so refusing it would reject the documented invocation (rosf's
    // own repro passes `--automaton placeholder`). It is warned about instead of refused.
    let source = std::fs::read_to_string(&args.context)
        .map_err(|e| format!("failed to read '{}': {e}", args.context.display()))?;
    if let Some(a) = args.adapter.as_deref()
        && a != "tlsf"
    {
        return Err(format!(
            "--controller-mode gr1 currently supports --adapter tlsf, got '{a}'"
        ));
    }
    let ir = mununu_core::adapter::tlsf::translate_to_ir(
        &source,
        &mununu_core::adapter::AdapterOptions::default(),
    )
    .map_err(|e| format!("TLSF translation failed: {e}"))?;

    let synth = mununu_core::adapter::gr1_synth::synthesise_gr1_from_ir(&ir, "gr1_controller")?;

    println!("GR(1) controller synthesis ({}):", args.context.display());
    if !args.automaton.is_empty() {
        // Visible rather than silent, and a warning rather than an error, because clap REQUIRES the
        // flag for this subcommand — see the note above `read_to_string`.
        println!(
            "  note: --automaton '{}' is ignored on the gr1 path (the game comes from the TLSF \
             spec, not a named automaton); it is required by the subcommand, so it is warned about \
             rather than refused",
            args.automaton
        );
    }
    println!(
        "  Realizable: {}",
        if synth.realizable { "yes" } else { "no" }
    );
    println!(
        "  Game: {} states, {} monitor bit(s)",
        synth.n_game_states, synth.n_monitor_bits
    );
    for note in &synth.notes {
        println!("  note: {note}");
    }
    if let Some(sv) = &synth.controller_sv {
        if let Some(path) = &args.emit_sv {
            std::fs::write(path, sv)
                .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
            println!("  Controller SystemVerilog written to {}", path.display());
        } else {
            println!(
                "  Controller synthesized ({} lines of SV); pass --emit-sv <FILE> to write it",
                sv.lines().count()
            );
        }
    } else if synth.realizable {
        println!("  (no controller emitted — see notes)");
    }
    // mununu#541 — the machine-readable contract rosf consumes. Written LAST so a write failure
    // cannot mask a synthesis error, and reported so the file's absence is never silent: the defect
    // this closes was `--dump-json` accepted, exit 0, and no file on disk.
    if let Some(path) = args.dump_json.as_ref() {
        let doc = serde_json::json!({
            "verb": "context-synth",
            "controller_mode": "gr1",
            "source": args.context.display().to_string(),
            "realizable": synth.realizable,
            "game": {
                "states": synth.n_game_states,
                "monitor_bits": synth.n_monitor_bits,
            },
            "controller": {
                "emitted": synth.controller_sv.is_some(),
                "sv_lines": synth.controller_sv.as_ref().map(|sv| sv.lines().count()),
                "written_to": args.emit_sv.as_ref().map(|p| p.display().to_string()),
            },
            "notes": synth.notes,
        });
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|e| format!("failed to serialise the synthesis report: {e}"))?;
        std::fs::write(path, text)
            .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
        println!("  JSON report written to {}", path.display());
    }
    Ok(())
}

fn context_synthesize(args: ContextSynthesizeArgs) -> Result<(), String> {
    // GR(1) synthesis takes a different path: it needs the STRUCTURED LTL spec
    // (assumptions + guarantees + signal directions) from the adapter IR, not
    // the combined μ-calculus formula the standard path realizes.
    let cli_mode = parse_cli_controller_mode(&args.controller_mode, args.extract_strategy)?;
    if cli_mode == mununu_core::context::ControllerMode::Gr1 {
        return context_synthesize_gr1(&args);
    }
    let preprocessor = validate_preprocessor(args.preprocessor.as_deref())?;
    let PreparedEvalContext {
        realized,
        formula_name,
    } = prepare_eval_context(EvalContextParams {
        context: &args.context,
        sidecars: &args.sidecars,
        adapter: args.adapter.as_deref(),
        mode: args.mode.as_deref(),
        preprocessor,
        print_ctxdsl_path: args.print_ctxdsl.as_ref(),
        stubs: &[], // context_synthesize has no --stub support
        formula: &args.formula,
        template: &args.template,
        template_args: &args.template_args,
        templates_files: &args.templates_files,
        automaton: &args.automaton,
        allow_no_formula: false,
    })?;
    let realized_formula = realized
        .formulas
        .get(&formula_name)
        .ok_or_else(|| format!("unknown formula '{}' in realised context", formula_name))?;
    if realized.context.clts(&args.automaton).is_none() {
        return Err(format!(
            "unknown automaton '{}' in realised context",
            args.automaton
        ));
    }
    let env = realized.environment_for(&args.automaton);

    let mut eval_options = EvaluationOptions::default();
    if args.no_partitions {
        eval_options.use_partitions = false;
    }

    let mut diagnostics = DiagnosticsOptions::default();
    let mut diagnostics_enabled = false;
    if args.counterexample {
        diagnostics.counterexample = true;
        diagnostics_enabled = true;
    }
    if args.deadlock_traces {
        diagnostics.deadlock_traces = true;
        diagnostics_enabled = true;
    }
    if let Some(limit) = args.max_counter_traces {
        diagnostics.max_counter_traces = Some(limit);
        diagnostics_enabled = true;
    }
    if args.no_proof_obligations {
        diagnostics.proof_obligations = false;
        diagnostics_enabled = true;
    }

    // Always enable diagnostics if proof_obligations is true (default), to ensure they are generated
    // even when no other diagnostics flags are set
    if diagnostics.proof_obligations {
        diagnostics_enabled = true;
    }

    let diagnostics_ref = diagnostics_enabled.then_some(diagnostics);

    let synthesis = realized
        .context
        .synthesise_controller_with_options(
            &args.automaton,
            &realized_formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: Some(&eval_options),
                diagnostics: diagnostics_ref.as_ref(),
                minimize: args.minimize,
                extract_strategy: args.extract_strategy,
                mode: parse_cli_controller_mode(&args.controller_mode, args.extract_strategy)?,
            },
        )
        .map_err(|err| format!("controller synthesis failed: {err}"))?;

    let controller = &synthesis.controller;
    let mut controllable_alphabet = controller.alphabet();
    controllable_alphabet.sort();
    let initial_count = controller.initial_states().len();
    let state_count = controller.state_count();

    println!(
        "Controller synthesis for formula '{}' over automaton '{}':",
        formula_name, args.automaton
    );
    println!(
        "  Realizable: {}",
        if synthesis.realizable { "yes" } else { "no" }
    );
    println!(
        "  Controller states: {} (initial: {})",
        state_count, initial_count
    );
    println!(
        "  Alphabet: {}",
        if controllable_alphabet.is_empty() {
            "(none)".to_owned()
        } else {
            controllable_alphabet.join(", ")
        }
    );
    println!(
        "  Structural hash: 0x{hash:016x}",
        hash = controller.structural_hash()
    );

    render_controller_diagnostics(&synthesis.diagnostics);

    // Soundness trust-level warning for liveness on over-approximate models
    if realized_formula.alternation_depth >= 2 {
        eprintln!(
            "  [SOUNDNESS WARNING] Trust level: LOW — formula has alternation depth {} \
             (class: {:?}). The winning region is correct, but the positional controller \
             may not cycle through obligations for liveness/GR(1) properties.",
            realized_formula.alternation_depth, realized_formula.property_class
        );
    }

    if args.soundness_report {
        if let Some(synth_clts) = realized.context.clts(&args.automaton) {
            print_soundness_report(&formula_name, realized_formula, synth_clts);
        } else {
            eprintln!(
                "  warning: automaton '{}' not found — skipping soundness report",
                args.automaton
            );
        }
    }

    if let Some(path) = args.dump_json.as_ref() {
        write_controller_json(path, &args.automaton, &formula_name, &synthesis)?;
        println!("  JSON summary written to {}", path.display());
    }

    if let Some(path) = args.emit_dsl.as_ref() {
        write_controller_ctxdsl(
            path,
            &args.automaton,
            &formula_name,
            realized_formula.raw.as_str(),
            controller,
        )?;
        println!("  Controller DSL written to {}", path.display());
    }

    if let Some(path) = args.dump_diagnostics.as_ref() {
        ensure_parent_dir(path)
            .map_err(|err| format!("failed to prepare diagnostics path: {err}"))?;
        synthesis
            .diagnostics
            .write_sidecar_dsl(path)
            .map_err(|err| format!("failed to write diagnostics sidecar: {err}"))?;
        println!("  Diagnostics sidecar written to {}", path.display());
    }

    // Emit controller in native format if requested
    if let Some(format) = args.output_format.as_deref() {
        if synthesis.realizable {
            let native_content = match format {
                "xstate" => {
                    use mununu_core::adapter::xstate::emit_controller::controller_to_xstate_json;
                    controller_to_xstate_json(controller, &args.automaton, true)
                }
                "systemverilog" | "sv" => {
                    use mununu_core::adapter::systemverilog::emit_controller::controller_to_systemverilog;
                    controller_to_systemverilog(controller, &args.automaton, true)
                }
                // CTXDSL Phase 3 (2026-06-22) — `--output-format ctxdsl`
                // now emits the controller CTXDSL (routed below to
                // `--emit-native` / stdout, like xstate / sv), reusing the
                // same emitter as `--emit-dsl`. Previously this returned an
                // empty string and silently produced no output.
                "ctxdsl" => controller_ctxdsl_string(
                    &args.automaton,
                    &formula_name,
                    realized_formula.raw.as_str(),
                    controller,
                )?,
                other => {
                    return Err(format!(
                        "unknown output format '{other}'. Supported: ctxdsl, xstate, systemverilog"
                    ));
                }
            };

            if !native_content.is_empty() {
                if let Some(path) = args.emit_native.as_ref() {
                    ensure_parent_dir(path)
                        .map_err(|err| format!("failed to prepare native output path: {err}"))?;
                    std::fs::write(path, &native_content)
                        .map_err(|err| format!("failed to write native controller: {err}"))?;
                    println!("  Controller ({format}) written to {}", path.display());
                } else {
                    println!("\n--- Controller ({format}) ---\n{native_content}");
                }
            }
        } else {
            eprintln!("  Note: --output-format ignored (specification is unrealizable)");
        }
    }

    // Print structure if requested
    if let Some(output_path) = args.print_structure {
        print_context_structure(&realized.context, output_path)?;
    }

    Ok(())
}

fn print_soundness_report(
    formula_name: &str,
    formula: &mununu_core::context_dsl::RealizedFormula,
    clts: &Clts<mununu_core::clts::DefaultStateIdx, DefaultLabelIdx>,
) {
    let has_noop = clts.states().any(|state| {
        clts.outgoing(state).iter().any(|t| {
            t.target() == state
                && t.labels().iter().any(|l| {
                    clts.label_payload(*l)
                        .is_some_and(|syms| syms.iter().any(|s| s == "noop" || s == "tau"))
                })
        })
    });

    let abstraction_dir = if has_noop {
        "over-approximation (noop/tau self-loops present)"
    } else {
        "exact (no detected over-approximation artifacts)"
    };

    let trust = match (formula.alternation_depth, has_noop) {
        (0, _) => "HIGH — propositional formula, no fixpoint semantics",
        (1, _) => "HIGH — safety/reachability (alternation depth 1), memoryless strategy is sound",
        (_, true) => "LOW — liveness on over-approximate model; verdict may not transfer",
        (_, false) => {
            "MEDIUM — liveness formula; winning region correct but controller is best-effort"
        }
    };

    println!("\n  ─── Soundness Report ───");
    println!("  Property:          {formula_name}");
    println!("  Class:             {:?}", formula.property_class);
    println!("  Alternation depth: {}", formula.alternation_depth);
    println!("  Abstraction:       {abstraction_dir}");
    println!("  Trust level:       {trust}");

    if formula.alternation_depth >= 2 {
        println!("  Recommendation:    Verify liveness claims against the real system.");
        println!(
            "                     Consider adding fairness constraints for async compositions."
        );
    }
    println!();
}

fn context_graph(args: ContextGraphArgs) -> Result<(), String> {
    let (context_doc, sidecar_docs, _) =
        load_context_documents(&args.context, &args.sidecars, None)?;
    let realized = realize_documents(&context_doc, &sidecar_docs)?;

    // Collect all automata to visualize
    let mut automata_to_visualize: Vec<String> = Vec::new();
    if let Some(automaton_name) = &args.automaton {
        if realized.context.clts(automaton_name).is_none() {
            return Err(format!(
                "unknown automaton '{}' in realised context",
                automaton_name
            ));
        }
        automata_to_visualize.push(automaton_name.clone());
    } else {
        // Get all automata from the context
        for doc in std::iter::once(&context_doc).chain(sidecar_docs.iter()) {
            for automaton in &doc.automata {
                if realized.context.clts(&automaton.name.name).is_some()
                    && !automata_to_visualize.contains(&automaton.name.name)
                {
                    automata_to_visualize.push(automaton.name.name.clone());
                }
            }
        }
    }

    if automata_to_visualize.is_empty() {
        return Err("no automata found to visualize".to_string());
    }

    // Generate Cytoscape elements based on output type
    let mut all_elements = Vec::new();

    match args.r#type {
        GraphOutputType::Dsl => {
            let elements = dsl_automata_to_cytoscape(
                &context_doc,
                &sidecar_docs,
                &realized,
                &automata_to_visualize,
            )?;
            all_elements.extend(elements);
        }
        GraphOutputType::Unrolled => {
            let elements = unrolled_automata_to_cytoscape(
                &context_doc,
                &sidecar_docs,
                &realized,
                &automata_to_visualize,
            )?;
            all_elements.extend(elements);
        }
        GraphOutputType::Both => {
            let dsl_elements = dsl_automata_to_cytoscape(
                &context_doc,
                &sidecar_docs,
                &realized,
                &automata_to_visualize,
            )?;
            all_elements.extend(dsl_elements);
            // Skip unrolled graph if automata have no variables to unroll
            if let Ok(unrolled_elements) = unrolled_automata_to_cytoscape(
                &context_doc,
                &sidecar_docs,
                &realized,
                &automata_to_visualize,
            ) {
                all_elements.extend(unrolled_elements);
            }
        }
    }

    // Generate counterstrategy graph if requested
    if args.counterstrategy {
        let formula_name = args
            .formula
            .as_ref()
            .ok_or("--counterstrategy requires --formula")?;
        let automaton_name = args
            .automaton
            .as_ref()
            .ok_or("--counterstrategy requires --automaton")?;

        let rf = realized
            .formulas
            .get(formula_name)
            .ok_or_else(|| format!("unknown formula '{formula_name}'"))?;
        let clts = realized
            .context
            .clts(automaton_name)
            .ok_or_else(|| format!("unknown automaton '{automaton_name}'"))?;
        let env = realized.environment_for(automaton_name);
        let eval_options = mununu_core::mu_calculus::EvaluationOptions::default();

        // Invert formula and evaluate to get environment winning region
        let inverted = mununu_core::mu_calculus::invert::invert(&rf.formula);
        let inv_bv = realized
            .context
            .evaluate_mu(automaton_name, &inverted, &env, Some(&eval_options))
            .map_err(|e| format!("counterstrategy evaluation failed: {e}"))?;

        let winning_set: std::collections::HashSet<usize> = clts
            .states()
            .filter(|sid| inv_bv.get(sid.index()).map(|bit| *bit).unwrap_or(false))
            .map(|sid| sid.index())
            .collect();

        let cs_name = format!("{automaton_name}_counterstrategy");
        let cs_cytoscape = counterstrategy_to_cytoscape(clts, &cs_name, &winning_set);

        all_elements.extend(cs_cytoscape);

        let winning_names: Vec<String> = clts
            .states()
            .filter(|sid| winning_set.contains(&sid.index()))
            .filter_map(|sid| clts.state_name(sid).map(|n| n.to_string()))
            .collect();
        println!(
            "Counterstrategy: environment wins from {} states: {}",
            winning_names.len(),
            winning_names.join(", ")
        );
    }

    // Generate HTML
    let html = generate_cytoscape_html(&all_elements)?;

    // Write to file
    ensure_parent_dir(&args.output)
        .map_err(|e| format!("failed to create output directory: {}", e))?;
    fs::write(&args.output, html).map_err(|e| {
        format!(
            "failed to write output file '{}': {}",
            args.output.display(),
            e
        )
    })?;

    println!("Graph visualization written to {}", args.output.display());

    Ok(())
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct ControllerSynthesisJson {
    automaton: String,
    formula: String,
    /// The verdict envelope: `holds` iff a controller exists.
    outcome: &'static str,
    realizable: bool,
    controller: ControllerJson,
    diagnostics: Value,
}

#[derive(Serialize)]
struct ControllerJson {
    state_count: usize,
    initial_state_count: usize,
    structural_hash: u64,
    controllable_alphabet: Vec<String>,
    states: Vec<ControllerStateJson>,
    transitions: Vec<ControllerTransitionJson>,
}

#[derive(Serialize)]
struct ControllerStateJson {
    name: String,
    initial: bool,
    variables: Vec<String>,
}

#[derive(Serialize)]
struct ControllerTransitionJson {
    source: String,
    target: String,
    controllable: bool,
    labels: Vec<Vec<String>>,
}

fn write_controller_json(
    path: &Path,
    automaton: &str,
    formula: &str,
    synthesis: &ControllerSynthesis,
) -> Result<(), String> {
    ensure_parent_dir(path)
        .map_err(|err| format!("failed to prepare JSON output directory: {err}"))?;

    let controller = &synthesis.controller;
    let mut controllable_alphabet = controller.alphabet();
    controllable_alphabet.sort();

    let initial_states = controller.initial_states();
    let mut states = Vec::new();
    for state in controller.states() {
        let idx = state.index();
        let name = controller
            .state_name(state)
            .map(|name| name.to_owned())
            .unwrap_or_else(|| format!("state_{idx}"));
        let variables = controller.state_variables(state);
        states.push(ControllerStateJson {
            name,
            initial: initial_states.contains(&state),
            variables,
        });
    }
    states.sort_by(|a, b| a.name.cmp(&b.name));

    let mut transitions = Vec::new();
    for state in controller.states() {
        let source_name = controller
            .state_name(state)
            .map(|name| name.to_owned())
            .unwrap_or_else(|| format!("state_{}", state.index()));
        for transition in controller.outgoing(state) {
            let target = transition.target();
            let target_name = controller
                .state_name(target)
                .map(|name| name.to_owned())
                .unwrap_or_else(|| format!("state_{}", target.index()));
            let mut labels: Vec<Vec<String>> = Vec::new();
            for label_id in transition.labels() {
                let mut payload = controller
                    .label_payload(*label_id)
                    .map(|values| values.to_vec())
                    .unwrap_or_default();
                payload.sort();
                labels.push(payload);
            }
            transitions.push(ControllerTransitionJson {
                source: source_name.clone(),
                target: target_name,
                controllable: transition.is_controllable(controller),
                labels,
            });
        }
    }
    transitions.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
    });

    let payload = ControllerSynthesisJson {
        automaton: automaton.to_owned(),
        formula: formula.to_owned(),
        outcome: mununu_core::api::models::realizable_outcome(synthesis.realizable),
        realizable: synthesis.realizable,
        controller: ControllerJson {
            state_count: controller.state_count(),
            initial_state_count: initial_states.len(),
            structural_hash: controller.structural_hash(),
            controllable_alphabet,
            states,
            transitions,
        },
        diagnostics: synthesis.diagnostics.to_json_value(),
    };

    let rendered = serde_json::to_string_pretty(&payload)
        .map_err(|err| format!("failed to serialise controller summary: {err}"))?;
    fs::write(path, rendered).map_err(|err| format!("failed to write controller JSON: {err}"))?;
    Ok(())
}

fn write_controller_ctxdsl(
    path: &Path,
    automaton: &str,
    formula: &str,
    raw_formula: &str,
    controller: &Clts,
) -> Result<(), String> {
    ensure_parent_dir(path)
        .map_err(|err| format!("failed to prepare DSL output directory: {err}"))?;
    let dsl = controller_ctxdsl_string(automaton, formula, raw_formula, controller)?;
    std::fs::write(path, dsl).map_err(|err| format!("failed to write controller DSL: {err}"))
}

/// CTXDSL Phase 3 (2026-06-22) — build the synthesised controller's CTXDSL
/// as a `String`. Returned by value so both `--emit-dsl <FILE>` and
/// `--output-format ctxdsl` (which routes to `--emit-native <FILE>` or
/// stdout) emit byte-for-byte identical controller CTXDSL. This is the
/// CLI's hand-authored 2-valued emitter; synthesised controllers are Sharp
/// / 2-valued by construction, so the predicate-cube `clts_to_ir` bridge
/// (modality + 3-valued labels) is not needed here.
fn controller_ctxdsl_string(
    automaton: &str,
    formula: &str,
    raw_formula: &str,
    controller: &Clts,
) -> Result<String, String> {
    use std::fmt::Write as _;
    let mut out = String::new();

    let mut ordered_labels: BTreeMap<usize, LabelId<DefaultLabelIdx>> = BTreeMap::new();
    for state in controller.states() {
        for transition in controller.outgoing(state) {
            for label_id in transition.labels() {
                ordered_labels.entry(label_id.index()).or_insert(*label_id);
            }
        }
    }

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
        let mut ident = sanitize_identifier_cli(&base);
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
        let mut ident = sanitize_identifier_cli(&raw);
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

    let context_ident = sanitize_identifier_cli(&format!("{}_{}_controller", automaton, formula));
    let automaton_ident = format!("{context_ident}_automaton");

    writeln!(
        out,
        "// Synthesised controller derived from automaton '{}' and formula '{}'",
        automaton, formula
    )
    .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "context {context_ident} {{")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;

    if !label_entries.is_empty() {
        writeln!(out, "    alphabet {{")
            .map_err(|err| format!("failed to write controller DSL: {err}"))?;
        for (_, ident, payload) in &label_entries {
            if payload.is_empty() {
                writeln!(out, "        label {ident}; // ε")
                    .map_err(|err| format!("failed to write controller DSL: {err}"))?;
            } else {
                writeln!(
                    out,
                    "        label {ident}; // original symbols: {}",
                    payload.join(", ")
                )
                .map_err(|err| format!("failed to write controller DSL: {err}"))?;
            }
        }
        writeln!(out, "    }}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    }

    writeln!(out, "    automata {{")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "        automaton {automaton_ident} {{")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "            states {{")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
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
        writeln!(out, "{line}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    }
    writeln!(out, "            }}")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "            transitions {{")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
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
            writeln!(out, "{line}")
                .map_err(|err| format!("failed to write controller DSL: {err}"))?;
        }
    }
    writeln!(out, "            }}")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "        }}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "    }}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "    mu_formulas {{")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(
        out,
        "        formula {} {{",
        sanitize_identifier_cli(formula)
    )
    .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "            over {automaton_ident};")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "            body = {raw_formula};")
        .map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "        }}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "    }}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    writeln!(out, "}}").map_err(|err| format!("failed to write controller DSL: {err}"))?;
    Ok(out)
}

fn sanitize_identifier_cli(value: &str) -> String {
    let mut ident: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if ident.is_empty() {
        ident.push_str("mediator");
    }
    if ident.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        ident.insert(0, '_');
    }
    ident
}

// ---------------------------------------------------------------------------
// Property templates CLI
// ---------------------------------------------------------------------------

fn handle_library(command: LibraryCommand) -> Result<(), String> {
    use mununu_core::library;

    match command {
        LibraryCommand::List => {
            println!("Shipped parameterised CTXDSL component templates:");
            println!();
            for t in library::templates() {
                println!("  {:<18} — {}", t.name, t.summary);
            }
            println!();
            println!("Emit with: `mununu library emit <NAME> [--instance-id ID] [-o PATH]`");
            Ok(())
        }
        LibraryCommand::Emit(args) => {
            let t = library::lookup(&args.name).ok_or_else(|| {
                let names: Vec<&str> = library::templates().iter().map(|t| t.name).collect();
                format!(
                    "unknown library template `{}`. Available: {}",
                    args.name,
                    names.join(", ")
                )
            })?;
            let body = library::emit(t, args.instance_id.as_deref());
            match args.output {
                Some(path) => std::fs::write(&path, &body)
                    .map_err(|e| format!("failed to write {}: {e}", path.display()))?,
                None => print!("{body}"),
            }
            Ok(())
        }
    }
}

fn list_templates(args: TemplatesArgs) -> Result<(), String> {
    use mununu_core::adapter::templates::{TemplateDomain, TemplateRegistry};

    let registry = TemplateRegistry::configured(&args.templates_file).map_err(|e| e.to_string())?;
    let origin_of = |t: &mununu_core::adapter::templates::PropertyTemplate| -> String {
        t.origin
            .as_ref()
            .map(|o| match (&o.pack, &o.file) {
                (Some(p), _) => format!("{} ({p})", o.layer),
                (None, Some(f)) => format!("{} ({f})", o.layer),
                (None, None) => o.layer.clone(),
            })
            .unwrap_or_else(|| "builtin".to_string())
    };

    // Show details for a single template
    if let Some(id) = &args.id {
        let tmpl = registry
            .get(id)
            .ok_or_else(|| format!("unknown template '{id}'"))?;
        if args.json {
            let json = serde_json::to_string_pretty(tmpl).map_err(|e| e.to_string())?;
            println!("{json}");
        } else {
            println!("Template: {} ({})", tmpl.id, tmpl.display_name);
            println!("  {}", tmpl.description);
            println!("  Kind: {}  Role: {}", tmpl.kind, tmpl.role);
            println!(
                "  Domains: {}",
                tmpl.domains
                    .iter()
                    .map(|d| format!("{d:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("  Origin: {}", origin_of(tmpl));
            if let Some(e) = tmpl.origin.as_ref().and_then(|o| o.entitlement.as_ref()) {
                println!("  Entitlement: {e} (declared; not enforced)");
            }
            if tmpl.params.is_empty() {
                println!("  Parameters: (none)");
            } else {
                println!("  Parameters:");
                for p in &tmpl.params {
                    let req = if p.required { "required" } else { "optional" };
                    let default = p
                        .default
                        .as_deref()
                        .map(|d| format!(" [default: {d}]"))
                        .unwrap_or_default();
                    println!("    ${} — {} ({req}{default})", p.name, p.description);
                }
            }
            println!("  Formula: {}", tmpl.formula_pattern);
            if !tmpl.tags.is_empty() {
                println!("  Tags: {}", tmpl.tags.join(", "));
            }
        }
        return Ok(());
    }

    // Every template, or one domain's plus the universal ones; the JSON follows the same filter.
    let domain_filter: Option<TemplateDomain> =
        args.domain.as_deref().map(str::parse).transpose()?;
    let mut templates = match domain_filter {
        Some(domain) => registry.for_domain(domain),
        None => registry
            .catalog()
            .templates
            .iter()
            .filter_map(|t| registry.get(&t.id))
            .collect(),
    };
    templates.sort_by_key(|t| t.id.clone());

    if args.json {
        let json = serde_json::to_string_pretty(&templates).map_err(|e| e.to_string())?;
        println!("{json}");
    } else {
        for tmpl in &templates {
            let params = if tmpl.params.is_empty() {
                String::new()
            } else {
                let names: Vec<_> = tmpl.params.iter().map(|p| format!("${}", p.name)).collect();
                format!("({})", names.join(", "))
            };
            println!(
                "  {}{:<24} {}  [{}]",
                tmpl.id,
                params,
                tmpl.description,
                origin_of(tmpl)
            );
        }
    }

    Ok(())
}

/// `--emit-testbench DIR`: one Verilator replay testbench per counterexample
/// ([`mununu_core::adapter::verilator::replay`]), under DIR/<property label or name>/.
fn emit_replay_testbenches(
    dir: &Path,
    args: &SvVerifyAutoArgs,
    sources: &[(String, String)],
    report: &mununu_core::adapter::slang::verify_auto::AutoVerifyReport,
) -> Result<(), String> {
    use mununu_core::adapter::verilator::replay::{ReplayDesign, counterexample_testbench};
    let clock = match &args.tb_clock {
        Some(c) => c.clone(),
        None => ["clk", "clk_i", "clock", "i_clk", "aclk", "clk_sys"]
            .iter()
            .find(|c| {
                sources[0]
                    .1
                    .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                    .any(|w| w == **c)
            })
            .map(|c| c.to_string())
            .ok_or("--emit-testbench: no clock port found; name it with --tb-clock")?,
    };
    // `rst_n=1` is the reset pinned INACTIVE: active is the other value.
    let reset = report.diagnostics.gated_resets.first().and_then(|g| {
        let (name, inactive) = g.split_once('=')?;
        let inactive: u64 = inactive.trim().parse().ok()?;
        Some((name.trim().to_string(), u64::from(inactive == 0)))
    });
    let file = args
        .lift
        .file
        .clone()
        .ok_or("--emit-testbench needs SV_FILE (the Makefile names the source files)")?;
    let paths: Vec<String> = std::iter::once(file.clone())
        .chain(args.lift.sources.iter().cloned())
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p).display().to_string())
        .collect();
    let top = args.lift.top.clone().unwrap_or_else(|| {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("top")
            .to_string()
    });
    let mut written = 0usize;
    for p in &report.properties {
        let Some(cex) = &p.counterexample else {
            continue;
        };
        let design = ReplayDesign {
            top: &top,
            clock: &clock,
            reset: reset.as_ref().map(|(n, a)| (n.as_str(), *a)),
            sources: &paths,
        };
        let Some(bench) = counterexample_testbench(&design, cex) else {
            continue;
        };
        let label: String = p
            .label
            .as_deref()
            .unwrap_or(&p.name)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let out = dir.join(&label);
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        std::fs::write(out.join("tb.cpp"), &bench.tb_cpp).map_err(|e| e.to_string())?;
        std::fs::write(out.join("Makefile"), &bench.makefile).map_err(|e| e.to_string())?;
        eprintln!(
            "replay testbench: {} ({} checks{})",
            out.display(),
            bench.checks,
            if bench.skipped.is_empty() {
                String::new()
            } else {
                format!(", {} cell(s) not compared", bench.skipped.len())
            }
        );
        written += 1;
    }
    if written == 0 {
        eprintln!("replay testbench: no counterexample with a path to replay");
    }
    let _ = sources;
    Ok(())
}

/// `--template ID` / `--template-arg [ID.]KEY=VALUE` → template references. A key is bound to its
/// template by the `ID.` prefix; an unprefixed key needs exactly one `--template`.
fn template_refs(
    templates: &[String],
    args: &[String],
) -> Result<Vec<mununu_core::adapter::templates::TemplateRef>, String> {
    let mut refs: Vec<mununu_core::adapter::templates::TemplateRef> = templates
        .iter()
        .map(|t| mununu_core::adapter::templates::TemplateRef {
            template: t.clone(),
            args: HashMap::new(),
        })
        .collect();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| format!("invalid --template-arg '{arg}': expected [ID.]KEY=VALUE"))?;
        let (target, key) = match key.split_once('.') {
            Some((id, k)) if templates.iter().any(|t| t == id) => (Some(id), k),
            _ => (None, key),
        };
        let idx = match target {
            Some(id) => refs.iter().position(|r| r.template == id),
            None if refs.len() == 1 => Some(0),
            None => {
                return Err(format!(
                    "--template-arg '{arg}': with several --template flags, prefix the key with \
                     the template's id (`{}.{key}=…`)",
                    templates.first().cloned().unwrap_or_default()
                ));
            }
        };
        let idx = idx.ok_or_else(|| format!("--template-arg '{arg}': no such --template"))?;
        refs[idx]
            .args
            .insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(refs)
}

/// Resolve the formula name for eval/synth: either from `--formula` or from `--template`.
///
/// When `--template` is provided, instantiates the template and injects a sidecar
/// CTXDSL document containing the formula so it can be found in the realized context.
fn resolve_formula_name(
    formula: &Option<String>,
    template: &Option<String>,
    template_args: &[String],
    templates_files: &[PathBuf],
    automaton: &str,
    sidecar_docs: &mut Vec<ContextDoc>,
) -> Result<String, String> {
    if let Some(name) = formula {
        return Ok(name.clone());
    }

    let template_id = template
        .as_ref()
        .ok_or("either --formula or --template is required")?;

    // Parse template args
    let mut args_map = HashMap::new();
    for arg in template_args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| format!("invalid --template-arg '{arg}': expected KEY=VALUE"))?;
        args_map.insert(key.to_string(), value.to_string());
    }

    let registry = mununu_core::adapter::templates::TemplateRegistry::configured(templates_files)
        .map_err(|e| e.to_string())?;
    let tref = mununu_core::adapter::templates::TemplateRef {
        template: template_id.clone(),
        args: args_map,
    };
    let inst = registry
        .instantiate(&tref)
        .map_err(|e| format!("template instantiation failed: {e}"))?;

    // Generate a sidecar CTXDSL document with the formula
    let formula_name = inst.name.clone();
    let sidecar_ctxdsl = format!(
        "context __template_sidecar {{\n  mu_formulas {{\n    formula {formula_name} {{\n      over {automaton};\n      body = {};\n    }}\n  }}\n}}\n",
        inst.formula
    );
    let sidecar_doc = parse_context_doc(&sidecar_ctxdsl)
        .map_err(|e| format!("internal error: generated template CTXDSL failed to parse: {e}"))?;
    sidecar_docs.push(sidecar_doc);

    eprintln!(
        "Template '{}' → formula '{}': {}",
        template_id, formula_name, inst.formula
    );

    Ok(formula_name)
}

/// GAP-009: returns a warning message when the model is small enough that
/// any verdict is vacuously satisfied, otherwise None.
///
/// A 1-state model can only produce 0/1 or 1/1 verdicts regardless of
/// formula content — the property never gets a chance to discriminate.
/// A 0-state model is the empty case (typically a result of upstream
/// extraction failure) — verdicts are nominally 0/0, also vacuous.
///
/// The threshold is intentionally tight (≤ 1). A 2+-state model can in
/// principle witness real behavior, even if some properties are still
/// trivially satisfied; that case is left for a follow-up predicate-level
/// vacuity check (out of scope for GAP-009).
fn vacuity_warning_for_state_count(state_count: usize) -> Option<String> {
    if state_count <= 1 {
        Some(format!(
            "[mununu] WARN: model has {state_count} reachable state(s); verdicts are \
             vacuously satisfied regardless of formula content. The property \
             may appear to hold (or fail) without genuinely witnessing or \
             excluding the modeled behavior. Confirm the model has \
             sufficient state-space distinction before relying on this verdict."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod vacuity_warning_tests {
    use super::vacuity_warning_for_state_count;

    /// 1-state and 0-state models trigger the vacuity warning.
    /// The MCP-001 / MCP-005 compositional validation pathology was
    /// per-instance automata collapsing to 1 state; the composed product
    /// stayed 1 state. Both verdicts (`no_clobber 0/1`, `clobber_reachable
    /// 1/1`) coincidentally matched the hand baselines but were vacuous.
    #[test]
    fn vacuity_warning_fires_for_one_state_model() {
        let warning = vacuity_warning_for_state_count(1)
            .expect("1-state models must trigger the vacuity warning");
        assert!(warning.contains("vacuously satisfied"));
        assert!(warning.contains("1 reachable state"));
    }

    #[test]
    fn vacuity_warning_fires_for_zero_state_model() {
        // The empty case — typically a result of upstream extraction
        // failure (e.g., class not found). Worth warning about even
        // though the user usually gets a different error first.
        let warning = vacuity_warning_for_state_count(0)
            .expect("0-state models must trigger the vacuity warning");
        assert!(warning.contains("vacuously satisfied"));
    }

    #[test]
    fn vacuity_warning_silent_for_normal_models() {
        // 2+ state models can witness real behavior; no warning.
        for n in [2, 4, 16, 100, 4096] {
            assert!(
                vacuity_warning_for_state_count(n).is_none(),
                "did not expect vacuity warning for {n}-state model",
            );
        }
    }
}

#[cfg(test)]
mod sv_validate_tests {
    //! C0.2 `mununu sv validate` glue. The lint core is unit-tested in
    //! `annotation::tests` (C0.1); these cover the exit-code / `--strict`
    //! wiring of the command.
    use super::{SvValidateArgs, sv_validate};
    use std::io::Write;

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("mununu_sv_validate_{name}.mununu.json"));
        std::fs::File::create(&p)
            .and_then(|mut f| f.write_all(body.as_bytes()))
            .expect("write temp sidecar");
        p
    }

    #[test]
    fn clean_sidecar_is_ok() {
        let p = write_tmp(
            "clean",
            r#"{"$schema":"mununu_sv_annotation_v1","module":"m"}"#,
        );
        let r = sv_validate(SvValidateArgs {
            sidecar: p.clone(),
            strict: false,
        });
        let _ = std::fs::remove_file(&p);
        assert!(r.is_ok(), "clean sidecar should validate: {r:?}");
    }

    #[test]
    fn typo_warns_but_passes_without_strict() {
        let p = write_tmp(
            "typo_lenient",
            r#"{"$schema":"mununu_sv_annotation_v1","module":"m","signals":[{"name":"x","abstration":"boolean"}]}"#,
        );
        let r = sv_validate(SvValidateArgs {
            sidecar: p.clone(),
            strict: false,
        });
        let _ = std::fs::remove_file(&p);
        assert!(r.is_ok(), "a typo warns but does not fail without --strict");
    }

    #[test]
    fn typo_fails_under_strict() {
        let p = write_tmp(
            "typo_strict",
            r#"{"$schema":"mununu_sv_annotation_v1","module":"m","signals":[{"name":"x","abstration":"boolean"}]}"#,
        );
        let r = sv_validate(SvValidateArgs {
            sidecar: p.clone(),
            strict: true,
        });
        let _ = std::fs::remove_file(&p);
        assert!(r.is_err(), "--strict must fail on an unknown-field warning");
    }

    #[test]
    fn removed_schema_hard_fails_even_without_strict() {
        let p = write_tmp(
            "removed",
            r#"{"$schema":"mununu_sv_multi_v1","module":"m"}"#,
        );
        let r = sv_validate(SvValidateArgs {
            sidecar: p.clone(),
            strict: false,
        });
        let _ = std::fs::remove_file(&p);
        assert!(r.is_err(), "a removed `$schema` must hard-fail");
    }
}

#[cfg(test)]
mod sv_discover_tests {
    //! C1.1 `mununu sv discover` skeleton-building core (the SV→BTOR2 +
    //! state-cell extraction is integration-tested in
    //! `adapter::yosys::tests`; this covers the pure skeleton shape).
    use super::build_discover_skeleton;
    use mununu_core::adapter::systemverilog::annotation::SignalAbstraction;
    use mununu_core::adapter::yosys::SvStateCell;

    #[test]
    fn skeleton_keeps_multi_bit_cells_and_omits_one_bit() {
        let cells = vec![
            SvStateCell {
                name: "u0.cnt".into(),
                width: 32,
            },
            SvStateCell {
                name: "u0.flag".into(),
                width: 1,
            },
            SvStateCell {
                name: "u0.state".into(),
                width: 2,
            },
        ];
        let sk = build_discover_skeleton("top", &cells);
        assert_eq!(sk.module, "top");
        assert_eq!(sk.schema.as_deref(), Some("mununu_sv_annotation_v1"));
        // Only the two multi-bit cells; the 1-bit `flag` is omitted.
        assert_eq!(sk.signals.len(), 2, "got {:?}", sk.signals);
        assert!(!sk.signals.iter().any(|s| s.name == "u0.flag"));
        let cnt = sk
            .signals
            .iter()
            .find(|s| s.name == "u0.cnt")
            .expect("cnt in skeleton");
        assert!(matches!(cnt.abstraction, SignalAbstraction::Ignored));
        assert!(cnt.note.as_deref().unwrap_or_default().contains("width=32"));
    }

    #[test]
    fn skeleton_for_all_one_bit_design_has_no_signals() {
        let cells = vec![SvStateCell {
            name: "st".into(),
            width: 1,
        }];
        let sk = build_discover_skeleton("fsm", &cells);
        assert!(
            sk.signals.is_empty(),
            "a pure 1-bit design needs no abstraction entries"
        );
    }
}

#[cfg(test)]
mod controller_ctxdsl_tests {
    //! CTXDSL Phase 3 (2026-06-22) — the synthesised-controller CTXDSL
    //! emitter `controller_ctxdsl_string` is shared by `--emit-dsl` and
    //! `--output-format ctxdsl` (the latter previously emitted nothing).
    use super::controller_ctxdsl_string;
    use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability};
    use mununu_core::context_dsl::parse;

    #[test]
    fn controller_ctxdsl_string_is_non_empty_and_parses() {
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        let s0 = b.state_id_or_insert("idle").expect("idle id");
        let s1 = b.state_id_or_insert("active").expect("active id");
        b.initial_state_id(s0);
        let go = b.labels().intern(["go"]).expect("go label");
        b.set_label_controllability(go, LabelControllability::Controllable);
        b.transition_ids(s0, &[go], s1);
        b.transition_ids(s1, &[go], s0);
        let controller = b.build().expect("build controller");

        // formula = NAME, raw_formula = the body text (matches the CLI call).
        let dsl = controller_ctxdsl_string("plant", "safety", "nu X. (<go> X)", &controller)
            .expect("emit controller ctxdsl");
        assert!(!dsl.is_empty(), "controller ctxdsl must be non-empty");
        assert!(dsl.contains("context "), "{dsl}");
        assert!(dsl.contains("automaton "), "{dsl}");
        assert!(dsl.contains("mu_formulas {"), "{dsl}");
        assert!(dsl.contains("transition "), "{dsl}");
        // The emitted controller CTXDSL is valid, re-loadable syntax.
        parse(&dsl).expect("emitted controller ctxdsl parses");
    }
}

#[cfg(test)]
mod json_output_contract_tests {
    use super::*;

    /// mununu#541 — every `sv` property verb must accept `--json`.
    ///
    /// rosf passed it because `sv verify-auto` established the idiom, got
    /// `error: unexpected argument '--json' found` (exit 2), and concluded there was no structured
    /// output at all. The success path was already JSON; only the flag was missing. This asserts
    /// clap ACCEPTS it on each verb, so the idiom cannot silently regress on one of them.
    #[test]
    fn every_sv_property_verb_accepts_json() {
        use clap::Parser;
        for argv in [
            vec![
                "mununu",
                "sv",
                "verify-recoverability",
                "d.sv",
                "--target",
                "s == 0",
                "--json",
            ],
            vec![
                "mununu",
                "sv",
                "verify-liveness",
                "d.sv",
                "--request",
                "a == 1",
                "--grant",
                "b == 1",
                "--json",
            ],
            vec![
                "mununu",
                "sv",
                "verify-liveness",
                "d.sv",
                "--response",
                "a == 1 => b == 1",
                "--json",
            ],
        ] {
            let parsed = Cli::try_parse_from(&argv);
            assert!(
                parsed.is_ok(),
                "`{}` must accept --json: {:?}",
                argv.join(" "),
                parsed.err().map(|e| e.to_string())
            );
        }
    }

    /// The JSON error document carries what a consumer needs to attribute the failure.
    ///
    /// `verb` and `file` matter because a CI lane runs many of these and reads one stream; an error
    /// document that said only `{"error": "..."}` would not say which invocation produced it.
    #[test]
    fn the_json_error_document_names_the_verb_and_the_file() {
        // `exit_with_json_error` diverges, so the shape is asserted on the same construction it
        // uses rather than by running it — the value here is pinning the FIELDS, and a test that
        // spawned a process to read them would be testing serde.
        let doc = serde_json::json!({
            "verb": "sv-verify-liveness",
            "file": "design.sv",
            "error": "could not build the liveness monitor",
        });
        for field in ["verb", "file", "error"] {
            assert!(
                doc.get(field).and_then(|v| v.as_str()).is_some(),
                "the error document must carry `{field}` so a consumer can attribute it"
            );
        }
    }
}

#[cfg(test)]
mod gr1_flag_guard_tests {
    use super::*;

    /// mununu#541 — the guard that makes the accepted-and-ignored class fail loudly.
    ///
    /// The gr1 path reads `context`, `adapter` and `emit_sv`. Every OTHER flag of the subcommand must
    /// be one of: honoured (`--dump-json`), refused by name (`GR1_UNSUPPORTED_FLAGS`), or the one
    /// documented structural exception (`--automaton`, which clap requires). A flag in none of those
    /// sets is silently ignored, which is the defect — `--dump-json <FILE>` accepted, exit 0, no file.
    ///
    /// **This list is deliberately hand-written rather than derived**, because deriving it from the
    /// struct is what a reflective test would do and there is no reflection here: the point is that a
    /// HUMAN adding a field has to come here and choose. If this test fails after you add a flag, the
    /// fix is to decide — honour it or refuse it — not to add it to this list and move on.
    #[test]
    fn every_context_synth_flag_is_honoured_or_refused_on_the_gr1_path() {
        // Every long flag `context synth` accepts, as of this commit.
        const ALL_FLAGS: &[&str] = &[
            "--sidecar",
            "--adapter",
            "--mode",
            "--preprocessor",
            "--formula",
            "--template",
            "--template-arg",
            "--automaton",
            "--no-partitions",
            "--minimize",
            "--extract-strategy",
            "--controller-mode",
            "--counterexample",
            "--deadlock-traces",
            "--max-counter-traces",
            "--no-proof-obligations",
            "--dump-json",
            "--emit-dsl",
            "--emit-sv",
            "--dump-diagnostics",
            "--print-structure",
            "--print-ctxdsl",
            "--output-format",
            "--emit-native",
            "--soundness-report",
        ];
        // Read and acted upon by the gr1 path.
        const HONOURED: &[&str] = &["--adapter", "--controller-mode", "--emit-sv", "--dump-json"];
        // Required by clap, so warned about rather than refused. See `context_synthesize_gr1`.
        const STRUCTURAL: &[&str] = &["--automaton"];
        // `--extract-strategy` is a legacy alias consumed by `parse_cli_controller_mode` BEFORE the
        // gr1 dispatch, so it can never reach this path unset-but-meaningful.
        const CONSUMED_BEFORE_DISPATCH: &[&str] = &["--extract-strategy"];

        let refused: Vec<&str> = GR1_UNSUPPORTED_FLAGS.iter().map(|(n, _)| *n).collect();
        let undecided: Vec<&&str> = ALL_FLAGS
            .iter()
            .filter(|f| {
                !HONOURED.contains(f)
                    && !STRUCTURAL.contains(f)
                    && !CONSUMED_BEFORE_DISPATCH.contains(f)
                    && !refused.contains(*f)
            })
            .collect();
        assert!(
            undecided.is_empty(),
            "these `context synth` flags are neither honoured nor refused on the gr1 path, so they \
             are SILENTLY IGNORED — decide for each one: {undecided:?}"
        );

        // And every refusal must carry a reason, because "unsupported" alone sends the reader back
        // to the source to find out what to use instead.
        for (name, why) in GR1_UNSUPPORTED_FLAGS {
            assert!(
                why.len() > 20,
                "{name}'s refusal must say what to do instead, got {why:?}"
            );
        }
    }

    /// A flag the path cannot honour is an ERROR, not a no-op that exits 0.
    #[test]
    fn a_flag_the_gr1_path_cannot_honour_is_refused_by_name() {
        let args = ContextSynthesizeArgs {
            context: std::path::PathBuf::from("/nonexistent.tlsf"),
            sidecars: Vec::new(),
            adapter: Some("tlsf".into()),
            mode: None,
            preprocessor: None,
            formula: None,
            template: None,
            template_args: Vec::new(),
            templates_files: Vec::new(),
            automaton: "placeholder".into(),
            no_partitions: false,
            minimize: false,
            extract_strategy: false,
            controller_mode: Some("gr1".into()),
            counterexample: false,
            deadlock_traces: false,
            max_counter_traces: None,
            no_proof_obligations: false,
            dump_json: None,
            emit_dsl: Some(std::path::PathBuf::from("/tmp/x.ctxdsl")),
            emit_sv: None,
            dump_diagnostics: None,
            print_structure: None,
            print_ctxdsl: None,
            output_format: None,
            emit_native: None,
            soundness_report: false,
        };
        let err = context_synthesize_gr1(&args).expect_err("--emit-dsl must be refused");
        assert!(
            err.contains("--emit-dsl") && err.contains("--emit-sv"),
            "the refusal must name the flag AND what to use instead: {err}"
        );
        // It must fail on the flag, BEFORE trying to read the (nonexistent) source — otherwise the
        // user sees an unrelated IO error and never learns the flag was the problem.
        assert!(
            !err.contains("nonexistent"),
            "the flag check must precede the file read: {err}"
        );
    }

    /// A run that passes only supported flags is not refused.
    #[test]
    fn the_guard_does_not_fire_on_a_clean_invocation() {
        let args = ContextSynthesizeArgs {
            context: std::path::PathBuf::from("/nonexistent.tlsf"),
            sidecars: Vec::new(),
            adapter: Some("tlsf".into()),
            mode: None,
            preprocessor: None,
            formula: None,
            template: None,
            template_args: Vec::new(),
            templates_files: Vec::new(),
            automaton: "placeholder".into(),
            no_partitions: false,
            minimize: false,
            extract_strategy: false,
            controller_mode: Some("gr1".into()),
            counterexample: false,
            deadlock_traces: false,
            max_counter_traces: None,
            no_proof_obligations: false,
            dump_json: Some(std::path::PathBuf::from("/tmp/ok.json")),
            emit_dsl: None,
            emit_sv: None,
            dump_diagnostics: None,
            print_structure: None,
            print_ctxdsl: None,
            output_format: None,
            emit_native: None,
            soundness_report: false,
        };
        assert!(
            gr1_rejected_flags(&args).is_empty(),
            "nothing to refuse here"
        );
        // It then fails on the missing FILE, which is the honest next error.
        let err = context_synthesize_gr1(&args).expect_err("the source does not exist");
        assert!(
            err.contains("nonexistent"),
            "expected the file error, got {err}"
        );
    }
}

#[cfg(test)]
mod sv_lift_search_path_tests {
    //! mununu#475 item 3 — `--search-path <DIR>` discovers and stages every
    //! `.v` / `.sv` under DIR alongside the primary file, so a single-file
    //! lift with cross-directory submodule instantiations resolves without
    //! the caller hand-listing every `--source`. Deduplicates against the
    //! primary and explicit `--source` entries by canonical path.
    use super::{SvFrontendArg, SvLiftArgs, read_sv_lift};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn args(
        file: Option<PathBuf>,
        sources: Vec<PathBuf>,
        search_paths: Vec<PathBuf>,
        exclude: Vec<String>,
    ) -> SvLiftArgs {
        SvLiftArgs {
            file,
            design_dir: None,
            exclude,
            search_paths,
            sources,
            top: None,
            merge_clock_domains: false,
            include_dirs: Vec::new(),
            frontend: SvFrontendArg::Auto,
            params: Vec::new(),
            lift_override: Vec::new(),
            cutpoint: Vec::new(),
        }
    }

    #[test]
    fn search_path_discovers_cross_directory_submodules() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        // Reporter's shape: mem_sched.sv in one dir, slot_arbiter.sv in a peer dir.
        for (rel, body) in [
            ("mem_sched/mem_sched.sv", "module mem_sched; endmodule\n"),
            ("lib/slot_arbiter.sv", "module slot_arbiter; endmodule\n"),
            ("lib/util.sv", "module util; endmodule\n"),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        let a = args(
            Some(root.join("mem_sched/mem_sched.sv")),
            Vec::new(),
            vec![root.join("lib")],
            Vec::new(),
        );
        let lift = read_sv_lift(&a).expect("read_sv_lift with --search-path");
        let names: Vec<&str> = lift
            .additional_sources
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(
            names.contains(&"slot_arbiter.sv"),
            "search-path should stage cross-dir modules: {names:?}"
        );
        assert!(
            names.contains(&"util.sv"),
            "search-path scans DIR recursively: {names:?}"
        );
    }

    #[test]
    fn search_path_dedupes_against_primary_and_source_entries() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        for (rel, body) in [
            ("primary.sv", "module top; endmodule\n"),
            ("explicit.sv", "module explicit; endmodule\n"),
            ("libdir/primary.sv", "module dup_top; endmodule\n"),
            ("libdir/explicit.sv", "module dup_explicit; endmodule\n"),
            ("libdir/unique.sv", "module unique; endmodule\n"),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        // Primary + one explicit --source; search-path libdir also contains
        // a file named `primary.sv` and one named `explicit.sv` — those are
        // DIFFERENT files on disk (different content), so canonical-path
        // dedup keeps both because they have distinct paths. The dedup only
        // fires if a search-path file IS the same file (same canonical path)
        // as the primary or an explicit source.
        let a = args(
            Some(root.join("primary.sv")),
            vec![root.join("explicit.sv")],
            vec![root.join("libdir")],
            Vec::new(),
        );
        let lift = read_sv_lift(&a).expect("read_sv_lift with --search-path + --source");
        // Now the same test but with libdir ALSO listing the primary file as a symlink /
        // duplicate. We use the primary itself via search-path to force a canonical-path
        // collision.
        let a2 = args(
            Some(root.join("primary.sv")),
            vec![root.join("explicit.sv")],
            vec![root.to_path_buf()],
            Vec::new(),
        );
        let lift2 = read_sv_lift(&a2).expect("read_sv_lift with root search-path");
        // The primary `primary.sv` and the explicit `explicit.sv` must not appear
        // twice — dedup by canonical path suppresses them from the search-path scan.
        let primary_count = lift2
            .additional_sources
            .iter()
            .filter(|(n, _)| n == "primary.sv")
            .count();
        let explicit_count = lift2
            .additional_sources
            .iter()
            .filter(|(n, _)| n == "explicit.sv")
            .count();
        assert_eq!(
            primary_count,
            0,
            "the primary file must NOT appear in additional_sources: {:?}",
            lift2
                .additional_sources
                .iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            explicit_count,
            1,
            "the explicit --source must appear exactly once: {:?}",
            lift2
                .additional_sources
                .iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        );
        // Sanity: the first lift found the libdir extras (different files).
        let names: Vec<&str> = lift
            .additional_sources
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(
            names.contains(&"unique.sv"),
            "libdir/unique.sv should be present: {names:?}"
        );
    }

    #[test]
    fn search_path_honours_exclude() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        for (rel, body) in [
            ("primary.sv", "module top; endmodule\n"),
            ("lib/util.sv", "module util; endmodule\n"),
            ("lib/faulty/bad.sv", "module bad; endmodule\n"),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        let a = args(
            Some(root.join("primary.sv")),
            Vec::new(),
            vec![root.join("lib")],
            vec!["faulty".to_string()],
        );
        let lift = read_sv_lift(&a).expect("read_sv_lift with --search-path --exclude");
        let names: Vec<&str> = lift
            .additional_sources
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(
            names.contains(&"util.sv"),
            "util.sv should stage: {names:?}"
        );
        assert!(
            !names.contains(&"bad.sv"),
            "--exclude faulty must also filter search-path scans: {names:?}"
        );
    }
}

/// mununu#543 W3 — the hidden child: evaluate ONE property with `exact-symbolic` and print a
/// single machine-readable verdict line.
///
/// Deliberately minimal, because everything it does not do is what makes it safe. It does not lift,
/// does not restrict a cone, does not pin a config and does not re-read the parent's options — all
/// of that is already baked into the BTOR2 it is handed. So there is no way for the child to verify
/// a different question than the parent asked, which is the failure mode that would make isolation
/// worse than the crash.
///
/// Output contract, read by `adapter::engine_isolate`:
///
/// ```text
/// VERDICT holds
/// VERDICT violated
/// VERDICT abstained <the engine's own reason>
/// ```
///
/// **No verdict line means the engine died**, which is the whole point: the parent reads the
/// absence plus the wait status rather than needing the child to report its own death.
///
/// The witness is not returned in this slice — a `violated` here carries no counterexample. Stated
/// in `engine_isolate`'s docs too, because a silently thinner result is worse than a stated one.
fn internal_engine_eval(args: InternalEngineEvalArgs) -> Result<(), String> {
    use mununu_core::adapter::btor2::symbolic_bitblast::{
        ExactSymbolicOptions, ExactVerdict, exact_symbolic_verdict_with_witness_and_options,
    };

    let btor2 = std::fs::read_to_string(&args.btor2).map_err(|e| {
        format!(
            "internal-engine-eval: cannot read {}: {e}",
            args.btor2.display()
        )
    })?;
    let formula = mununu_core::mu_calculus::parser::parse(&args.formula)
        .map_err(|e| format!("internal-engine-eval: cannot parse the formula: {e:?}"))?;
    let opts = ExactSymbolicOptions {
        antecedent_shadow_enabled: true,
        antecedent_propagate_enabled: !args.no_antecedent_propagate,
        input_state_enabled: !args.no_input_state,
        ..Default::default()
    };

    match exact_symbolic_verdict_with_witness_and_options(&btor2, &formula, &opts) {
        Ok((ExactVerdict::Holds, _)) => println!("VERDICT holds"),
        Ok((ExactVerdict::Violated, _)) => println!("VERDICT violated"),
        // An engine abstention is a RESULT, not a failure — it must reach the parent as one, or the
        // parent would read it as a death and report `unknown` without the engine's reason.
        Err(e) => println!("VERDICT abstained {e}"),
    }
    Ok(())
}

#[cfg(test)]
mod build_identity_tests {
    /// mununu#715 — the version line is the package version, then the short commit and a
    /// `-dirty` marker when the build knew them, and nothing else.
    #[test]
    fn the_version_line_carries_the_build_identity_when_known() {
        let line = env!("MUNUNU_VERSION_LINE");
        let version = env!("CARGO_PKG_VERSION");
        let rest = line
            .strip_prefix(version)
            .unwrap_or_else(|| panic!("`{line}` starts with the package version {version}"));
        if rest.is_empty() {
            assert!(
                option_env!("MUNUNU_BUILD_SHA").is_none(),
                "a known commit must appear in the version line"
            );
            return;
        }
        let inner = rest
            .strip_prefix(" (")
            .and_then(|r| r.strip_suffix(')'))
            .unwrap_or_else(|| panic!("`{rest}` is ` (<sha>[-dirty])`"));
        let sha = inner.strip_suffix("-dirty").unwrap_or(inner);
        assert_eq!(sha.len(), 7, "{inner}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{inner}");
        let Some(full) = option_env!("MUNUNU_BUILD_SHA") else {
            panic!("a commit in the version line must also be recorded as MUNUNU_BUILD_SHA");
        };
        assert!(full.starts_with(sha), "{full} vs {sha}");
        assert_eq!(
            inner.ends_with("-dirty"),
            option_env!("MUNUNU_BUILD_DIRTY") == Some("1")
        );
    }
}

#[cfg(test)]
mod sv_mutate_lift_flag_tests {
    use super::*;

    /// mununu#725 — `sv mutate` accepts the lift-posture flags `sv verify-auto` has
    /// (`--lift-override`, `--pin`, `--cutpoint`). Every OpenTitan IP lifts only with
    /// the `prim_assert.sv` shim and `rst_ni=1`, so without them a real design could not be
    /// mutated from the CLI at all (the full-IP mutation smoke had to go through the Rust API).
    #[test]
    fn sv_mutate_accepts_the_lift_posture_flags() {
        use clap::Parser;
        let argv = [
            "mununu",
            "sv",
            "mutate",
            "edn.sv",
            "--top",
            "edn",
            "--mutation",
            "stick:u_reg.q",
            "--lift-override",
            "prim_assert.sv=shim/prim_assert.sv",
            "--pin",
            "rst_ni=1",
            "--cutpoint",
            "must_refresh",
        ];
        let parsed = Cli::try_parse_from(argv);
        assert!(
            parsed.is_ok(),
            "`sv mutate` must accept the lift-posture flags (mununu#725): {:?}",
            parsed.err().map(|e| e.to_string())
        );
    }

    /// PR C — the k-fault adversary flags: `btor2 verify --fault … --target`, `btor2 game --fault`;
    /// `--target` needs `--fault`.
    #[test]
    fn fault_adversary_flags_parse() {
        for argv in [
            vec![
                "mununu",
                "btor2",
                "verify",
                "d.btor2",
                "--fault",
                "state_q:1",
                "--target",
                "state_q == 680",
            ],
            vec![
                "mununu", "btor2", "game", "d.btor2", "--good", "st == 2", "--fault", "st:1",
            ],
        ] {
            let parsed = Cli::try_parse_from(&argv);
            assert!(
                parsed.is_ok(),
                "{argv:?}: {:?}",
                parsed.err().map(|e| e.to_string())
            );
        }
        assert!(
            Cli::try_parse_from(["mununu", "btor2", "verify", "d.btor2", "--target", "x == 1"])
                .is_err(),
            "--target without --fault"
        );
    }

    /// PR A1 — the fault-injection posture flags parse on the three verbs and the file
    /// transform (`--fault` repeatable with `REG[:k]`, `--fault-model MODEL[:k]`,
    /// `--fault-warmup`); the "one of the two selections" rule is the core's, tested there.
    #[test]
    fn fault_posture_flags_parse_on_every_surface() {
        for argv in [
            vec![
                "mununu",
                "sv",
                "verify-auto",
                "t.sv",
                "--fault",
                "a",
                "--fault",
                "gen[1].q:2",
                "--fault-warmup",
                "3",
            ],
            vec![
                "mununu",
                "sv",
                "verify-auto",
                "t.sv",
                "--fault-model",
                "sec-cm:0",
            ],
            vec![
                "mununu",
                "sv",
                "mutate",
                "t.sv",
                "--mutation",
                "stick:a",
                "--fault-model",
                "sec-cm",
            ],
            vec![
                "mununu",
                "btor2",
                "verify",
                "d.btor2",
                "--fault",
                "state_q:1",
                "--fault-warmup",
                "0",
            ],
            vec![
                "mununu",
                "btor2",
                "inject-fault",
                "d.btor2",
                "--fault",
                "state_q",
                "-o",
                "out.btor2",
            ],
        ] {
            let parsed = Cli::try_parse_from(&argv);
            assert!(
                parsed.is_ok(),
                "{argv:?}: {:?}",
                parsed.err().map(|e| e.to_string())
            );
        }
        // `inject-fault` without a site is a usage error
        assert!(Cli::try_parse_from(["mununu", "btor2", "inject-fault", "d.btor2"]).is_err());
    }

    /// `--template-arg` binds by the `ID.` prefix; unprefixed needs a single `--template`.
    #[test]
    fn template_args_group_by_their_template() {
        let one = template_refs(&["rtl_never".into()], &["BAD=state_q == 5".into()]).unwrap();
        assert_eq!(one[0].args["BAD"], "state_q == 5");
        let two = template_refs(
            &["rtl_never".into(), "rtl_reachable".into()],
            &[
                "rtl_never.BAD=a == 1".into(),
                "rtl_reachable.TARGET=b == 2".into(),
            ],
        )
        .unwrap();
        assert_eq!(two[0].args["BAD"], "a == 1");
        assert_eq!(two[1].args["TARGET"], "b == 2");
        assert!(
            template_refs(
                &["rtl_never".into(), "rtl_reachable".into()],
                &["BAD=a == 1".into()]
            )
            .is_err()
        );
    }
}
