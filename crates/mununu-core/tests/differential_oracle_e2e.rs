//! Differential-oracle e2e suite (P1 seed) — plan: `.claude/plans/differential-oracle-e2e-suite.md`.
//!
//! **Principle.** No single-engine definite verdict is trusted; every DEFINITE verdict is
//! cross-checked against an INDEPENDENT oracle, and a disagreement is a test failure. This
//! inverts the "pin an expected verdict literal" pattern that let the #242 frozen-register
//! bug hide for weeks (the e2e tests asserted the buggy HOLDS instead of cross-checking).
//!
//! P1 is the **reachability differential**: the exact-symbolic engine's `EF(atom)` verdict
//! (reachable?) must agree with **btormc**'s bad-reachability of the same atom — btormc is an
//! independent external model checker on the same BTOR2, so a disagreement is a genuine
//! engine bug. This is exactly the differential that #242 would have failed: the frozen
//! register made the exact engine report a reachable state as UNREACHABLE, while btormc
//! (which never touches the BddBitBlaster) reports it reachable.
//!
//! Docker-gated (`mununu-sva`): needs btormc. Run with `--ignored`.

use mununu_core::adapter::btor2::symbolic_bitblast::{
    ExactVerdict, exact_bad_reachable, exact_symbolic_verdict,
};
use mununu_core::adapter::btormc::{
    DEFAULT_KMAX, DEFAULT_TIMEOUT, McVerdict, locate_btormc, run_btormc,
};
use mununu_core::adapter::slang::verify_auto::{
    PortfolioMode, VerifyAutoOptions, VerifyOutcome, verify_auto,
};
use mununu_core::adapter::yosys::YosysOptions;
use mununu_core::mu_calculus::parser;
use std::path::PathBuf;

/// A register that LOADS a nonzero value (10) from a free input `ld`, whose user-visible
/// name `reg` survives only on a `uext` alias of an UNNAMED state — the flattened-yosys shape
/// that triggered #242. `init reg = 0`; `bad = (reg == 10)`. From the reset state, asserting
/// `ld` reaches `reg = 10` in one step, so `reg == 10` IS reachable — both oracles must agree.
// Note: btormc's parser requires the `init` value's id to be BELOW the state's id, so the
// constants are declared before the state (unlike a typical yosys dump where the state is
// early). mununu's own parser is order-agnostic; this ordering keeps BOTH oracles happy.
const UEXT_ALIASED_LOAD_WITH_BAD: &str = r#"
1 sort bitvec 1
2 sort bitvec 4
3 const 2 0000
4 const 2 1010
5 input 1 ld
6 state 2
7 ite 2 5 4 6
8 uext 2 6 0 reg
9 eq 1 8 4
10 next 2 6 7
11 init 2 6 3
12 bad 9
"#;

/// The reachability differential, returned as a structured result so a failure is actionable.
struct ReachDifferential {
    exact_reachable: bool,
    btormc_reachable: bool,
}

impl ReachDifferential {
    fn agrees(&self) -> bool {
        self.exact_reachable == self.btormc_reachable
    }
}

/// Run `EF(atom)` through the exact engine and btormc's bad-reachability on the same BTOR2
/// (the `bad` line must encode `atom`), and report whether they agree that `atom` is reachable.
fn reachability_differential(btor2: &str, ef_formula: &str) -> ReachDifferential {
    let formula = parser::parse(ef_formula).expect("EF formula parses");
    let exact_reachable = matches!(
        exact_symbolic_verdict(btor2, &formula).expect("exact verdict"),
        ExactVerdict::Holds, // EF true ⇒ the atom is reachable from the init state
    );
    let bin = locate_btormc().expect("btormc present (mununu-sva)");
    let btormc_reachable = matches!(
        run_btormc(&bin, btor2, DEFAULT_KMAX, DEFAULT_TIMEOUT).expect("btormc runs"),
        McVerdict::Violated, // a reachable `bad` ⇒ the atom is reachable
    );
    ReachDifferential {
        exact_reachable,
        btormc_reachable,
    }
}

#[test]
#[ignore = "requires btormc (mununu-sva image); run with --ignored"]
fn diff_reachability_exact_vs_btormc_agree_on_uext_aliased_register() {
    // The #242-catcher. `reg == 10` is reachable (assert `ld` once from init `reg == 0`).
    // Post-fix both oracles say reachable; PRE-#242-fix the exact engine froze the register
    // and reported it UNREACHABLE while btormc reported it reachable — a divergence this
    // differential fails on. `reg` binds through the `uext` alias, the exact #242 shape.
    let diff = reachability_differential(UEXT_ALIASED_LOAD_WITH_BAD, "mu Y. ((reg == 10) or <> Y)");
    assert!(
        diff.btormc_reachable,
        "sanity: btormc must find `reg == 10` reachable (assert `ld` from init 0)"
    );
    assert!(
        diff.agrees(),
        "REACHABILITY DIFFERENTIAL FAILED: exact_reachable={} but btormc_reachable={} for \
         `reg == 10` on the uext-aliased register — the exact engine disagrees with the \
         independent btormc oracle (the #242 frozen-register signature).",
        diff.exact_reachable,
        diff.btormc_reachable,
    );
}

// ============================================================================
// HWMCC-style portfolio COVERAGE STUDY — how much of a real BTOR2 safety-benchmark
// suite the exact engine decides, and that every verdict it DOES emit agrees with
// btormc (the independent oracle). Not a pass/fail coverage gate (most real
// benchmarks are over the 40-bit bit-blast cap — an honest, documented limit); it
// IS a hard SOUNDNESS gate (an exact↔btormc disagreement on a definite verdict is a
// bug). Benchmarks are vendored byte-exact from btor2tools (MIT) — see
// examples/btor2/btor2tools_suite/PROVENANCE.md.
// ============================================================================

/// The vendored btor2tools suite, with the ground-truth verdict where the upstream
/// `-sat` / `-unsat` filename convention records it (`sat` = `bad` reachable).
const BTOR2TOOLS_SUITE: &[(&str, Option<bool>)] = &[
    ("count2.btor2", None),
    ("count4.btor2", None),
    ("recount4.btor2", None),
    ("twocount2.btor2", None),
    ("factorial4even.btor2", None),
    ("twocount32.btor2", None), // 65-bit (two 32-bit counters); exact abstains on the wall-clock deadline
    ("noninitstate.btor2", None), // has a `constraint` — the exact engine refuses (soundness)
    ("twocount2c.btor2", None), // has a `constraint`
    ("ponylink-slaveTXlen-sat.btor2", Some(true)), // 2870 cone bits — over the cap (excluded fast)
];

#[test]
#[ignore = "requires btormc (mununu-sva image); run with --ignored"]
fn hwmcc_style_coverage_study() {
    let bin = locate_btormc().expect("btormc present (mununu-sva)");
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/btor2/btor2tools_suite");

    let (mut decided, mut skipped, mut agree, mut total) = (0u32, 0u32, 0u32, 0u32);
    let mut disagreements: Vec<String> = Vec::new();
    let mut oracle_label_mismatch: Vec<String> = Vec::new();

    eprintln!(
        "\n===== HWMCC-style coverage study: exact engine ↔ btormc (real btor2tools suite) ====="
    );
    for (fname, known) in BTOR2TOOLS_SUITE {
        total += 1;
        let content = std::fs::read_to_string(root.join(fname))
            .unwrap_or_else(|e| panic!("read {fname}: {e}"));
        // Our exact engine's bad-reachability, and btormc's (the independent oracle).
        let ours = exact_bad_reachable(&content);
        let mc = run_btormc(&bin, &content, DEFAULT_KMAX, DEFAULT_TIMEOUT).expect("btormc runs");
        let mc_reach = match mc {
            McVerdict::Violated => Some(true), // bad reachable
            McVerdict::Safe => Some(false),    // proved unreachable
            McVerdict::Unknown => None,        // bounded-inconclusive
        };
        // Sanity on the oracle: btormc must match the benchmark's own -sat/-unsat label.
        if let (Some(lbl), Some(mcr)) = (known, mc_reach)
            && *lbl != mcr
        {
            oracle_label_mismatch.push(format!("{fname}: name={lbl} btormc={mcr}"));
        }

        match ours {
            Ok(r) => {
                decided += 1;
                match mc_reach {
                    Some(mcr) if mcr == r => agree += 1,
                    // Opposite definite verdicts ⇒ one engine is unsound. The prize.
                    Some(mcr) => {
                        disagreements.push(format!("{fname}: exact={r} but btormc reachable={mcr}"))
                    }
                    // btormc bounded-inconclusive; our definite verdict cannot be contradicted.
                    None => {}
                }
                eprintln!(
                    "  {fname:34} exact={:11}  btormc={mc:?}",
                    if r { "REACHABLE" } else { "unreachable" }
                );
            }
            Err(e) => {
                skipped += 1;
                eprintln!(
                    "  {fname:34} exact=SKIPPED      btormc={mc:?}   [{}]",
                    e.lines().next().unwrap_or("")
                );
            }
        }
    }
    eprintln!(
        "-----------------------------------------------------------------------------------"
    );
    eprintln!(
        "  coverage: exact decided {decided}/{total} (skipped/refused {skipped});  agreement with btormc {agree}/{decided};  disagreements {}",
        disagreements.len()
    );
    eprintln!(
        "===================================================================================\n"
    );

    // Hard soundness gate: a definite exact↔btormc contradiction is a genuine engine bug.
    assert!(
        disagreements.is_empty(),
        "EXACT↔BTORMC SOUNDNESS DISAGREEMENT: {disagreements:?} — the exact engine's \
         bad-reachability contradicts the independent btormc oracle."
    );
    // Sanity: btormc must agree with the vendored benchmarks' own ground-truth labels.
    assert!(
        oracle_label_mismatch.is_empty(),
        "btormc disagrees with a benchmark's -sat/-unsat label: {oracle_label_mismatch:?}"
    );
}

/// MAKE-CI adjudication of the IN-HOUSE engines over the vendored btor2tools suite
/// — the exact BDD engine ⊕ the native BMC/k-induction engine, both **in-process**
/// (Z3 / BDD, NO subprocess), so this runs in `make ci` (unlike the btormc study
/// above). Two gates: (1) a hard SOUNDNESS gate — wherever both engines decide they
/// must AGREE (a definite disagreement is an engine bug); (2) a LABEL gate — a
/// definite verdict must match the benchmark's own `-sat`/`-unsat` ground truth.
/// It also reports the native engine's CONTRIBUTION: benchmarks it decides where
/// the exact engine abstains (over its 40-bit cap) — the in-house scale win on
/// real-derived inputs.
#[test]
fn native_engine_adjudication_over_btor2tools_suite() {
    use mununu_core::adapter::btor2::native_bmc::{SafetyVerdict, decide_bad_safety};
    use mununu_core::adapter::btor2::parser::parse as parse_btor2;
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/btor2/btor2tools_suite");

    let (mut both, mut agree, mut native_only, mut exact_only) = (0u32, 0u32, 0u32, 0u32);
    let mut disagreements: Vec<String> = Vec::new();
    let mut label_mismatch: Vec<String> = Vec::new();
    eprintln!(
        "\n===== in-house adjudication: exact ⊕ native over the btor2tools suite (make-ci) ====="
    );
    for (fname, known) in BTOR2TOOLS_SUITE {
        let content = std::fs::read_to_string(root.join(fname))
            .unwrap_or_else(|e| panic!("read {fname}: {e}"));
        // Exact BDD engine: Some(bool) reachability, or None — abstains on over-cap (ponylink's
        // 2870 cone bits), free-init refusal (noninitstate), or the wall-clock deadline (the
        // 65-bit twocount32 counter, whose 2^32-diameter fixpoint the cap can't exclude).
        let exact = exact_bad_reachable(&content).ok();
        // Native engine: in-house BMC + k-induction, bounded k=40 + a 2s per-check
        // budget (the deciding benchmarks resolve in well under it; only the 228-bit
        // ponylink burns the budget, and it abstains anyway).
        let native = match parse_btor2(&content) {
            Ok(file) => match decide_bad_safety(&file, 40, Some(2_000)) {
                Ok(SafetyVerdict::Violated { .. }) => Some(true),
                Ok(SafetyVerdict::Safe { .. }) => Some(false),
                _ => None,
            },
            Err(_) => None,
        };
        // Soundness: where BOTH decide, they must agree.
        match (exact, native) {
            (Some(e), Some(n)) => {
                both += 1;
                if e == n {
                    agree += 1;
                } else {
                    disagreements.push(format!("{fname}: exact={e} native={n}"));
                }
            }
            (None, Some(_)) => native_only += 1,
            (Some(_), None) => exact_only += 1,
            (None, None) => {}
        }
        // Label gate: a definite verdict must match the benchmark's ground truth.
        if let Some(lbl) = known {
            for (eng, v) in [("exact", exact), ("native", native)] {
                if let Some(vv) = v
                    && *lbl != vv
                {
                    label_mismatch.push(format!("{fname}: {eng}={vv} vs label={lbl}"));
                }
            }
        }
        let show = |v: Option<bool>| match v {
            Some(true) => "REACHABLE",
            Some(false) => "unreachable",
            None => "abstain",
        };
        eprintln!(
            "  {fname:34} exact={:11}  native={}",
            show(exact),
            show(native)
        );
    }
    eprintln!(
        "-----------------------------------------------------------------------------------"
    );
    eprintln!(
        "  both-decided {both} (agree {agree});  native-only {native_only} (past the exact cap);  exact-only {exact_only}"
    );
    eprintln!(
        "===================================================================================\n"
    );

    // Hard SOUNDNESS gate — an exact↔native definite disagreement is a genuine bug.
    assert!(
        disagreements.is_empty(),
        "EXACT↔NATIVE SOUNDNESS DISAGREEMENT: {disagreements:?}"
    );
    // Label gate — a definite verdict must match the benchmark's -sat/-unsat label.
    assert!(
        label_mismatch.is_empty(),
        "a definite in-house verdict contradicts the benchmark label: {label_mismatch:?}"
    );
}

/// LOCAL-ONLY HWMCC adjudication harness — run the FULL reachability portfolio
/// (exact ⊕ native ⊕ btormc ⊕ Pono) over a directory of BTOR2 benchmarks the USER
/// provides, and gate on soundness + the benchmarks' own `-sat`/`-unsat` labels.
///
/// # Why a harness, not vendored benchmarks
///
/// The official HWMCC benchmark set (`hwmcc20benchmarks.tar.xz`) ships with **no
/// license** — redistributing it is a copyright risk, so mununu does NOT vendor it.
/// This ships the ADJUDICATOR; you provide the benchmarks — exactly like the
/// subprocess-tools-not-bundled policy ships the code, not the tools.
///
/// # How to run
///
/// ```sh
/// curl -LO https://hwmcc.github.io/2020/hwmcc20benchmarks.tar.xz
/// mkdir -p /tmp/hwmcc20 && tar xf hwmcc20benchmarks.tar.xz -C /tmp/hwmcc20
/// # in the mununu-sva image (btormc + pono on PATH), point at a leaf dir of .btor2:
/// MUNUNU_HWMCC_DIR=/tmp/hwmcc20/<btor-subdir> \
///   cargo test -p mununu-core --test differential_oracle_e2e \
///   hwmcc_adjudication_over_user_dir -- --ignored --nocapture
/// ```
///
/// Two HARD gates: (1) a [`ReachVerdict::Contradiction`] — two sound engines
/// disagreeing — is a soundness alarm; (2) a definite portfolio verdict that
/// contradicts the benchmark's `-sat`/`-unsat` filename label is a bug. It also
/// reports coverage (decided / abstained) and which engine carried each verdict.
/// Unset `MUNUNU_HWMCC_DIR` ⇒ the test no-ops (nothing to adjudicate).
#[test]
#[ignore = "provide MUNUNU_HWMCC_DIR (user-downloaded HWMCC benchmarks); run in mununu-sva"]
fn hwmcc_adjudication_over_user_dir() {
    use mununu_core::adapter::btor2::parser::parse as parse_btor2;
    use mununu_core::adapter::reach_portfolio::{ReachVerdict, decide_reach_portfolio};

    let Some(dir) = std::env::var_os("MUNUNU_HWMCC_DIR") else {
        eprintln!("MUNUNU_HWMCC_DIR unset — nothing to adjudicate (see the doc comment).");
        return;
    };
    let dir = PathBuf::from(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("btor2") | Some("btor")
            )
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no .btor2/.btor files in {}",
        dir.display()
    );

    let (mut total, mut decided, mut reach, mut unreach) = (0u32, 0u32, 0u32, 0u32);
    let mut contradictions: Vec<String> = Vec::new();
    let mut label_mismatch: Vec<String> = Vec::new();
    let mut by_engine: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    eprintln!(
        "\n===== HWMCC adjudication: full portfolio over {} =====",
        dir.display()
    );
    for path in &files {
        total += 1;
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let file = match parse_btor2(&content) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("  {fname:44} PARSE-ERROR");
                continue;
            }
        };
        let out = decide_reach_portfolio(&file);
        // Ground truth from the -sat/-unsat filename convention.
        let label = if fname.contains("-sat") {
            Some(true)
        } else if fname.contains("-unsat") {
            Some(false)
        } else {
            None
        };
        let verdict_bool = match out.verdict {
            ReachVerdict::Reachable => {
                decided += 1;
                reach += 1;
                Some(true)
            }
            ReachVerdict::Unreachable => {
                decided += 1;
                unreach += 1;
                Some(false)
            }
            ReachVerdict::Unknown => None,
            ReachVerdict::Contradiction => {
                contradictions.push(format!("{fname}: {out:?}"));
                None
            }
        };
        if let (Some(lbl), Some(v)) = (label, verdict_bool)
            && lbl != v
        {
            label_mismatch.push(format!("{fname}: portfolio={v} vs label={lbl}"));
        }
        for e in out.reachable_by.iter().chain(out.unreachable_by.iter()) {
            *by_engine.entry((*e).to_string()).or_default() += 1;
        }
        eprintln!(
            "  {fname:44} {:?}   reach_by={:?} unreach_by={:?}",
            out.verdict, out.reachable_by, out.unreachable_by
        );
    }
    eprintln!(
        "-----------------------------------------------------------------------------------"
    );
    eprintln!(
        "  benchmarks {total};  decided {decided} (reachable {reach}, unreachable {unreach});  abstained {}",
        total - decided
    );
    eprintln!("  per-engine contributions: {by_engine:?}");
    eprintln!(
        "===================================================================================\n"
    );

    // Hard SOUNDNESS gate — a portfolio contradiction is two sound engines disagreeing.
    assert!(
        contradictions.is_empty(),
        "SOUNDNESS ALARM — portfolio contradiction: {contradictions:?}"
    );
    // Hard LABEL gate — a definite verdict must match the benchmark's ground truth.
    assert!(
        label_mismatch.is_empty(),
        "a definite portfolio verdict contradicts the -sat/-unsat label: {label_mismatch:?}"
    );
}

/// IN-HOUSE timing runner for the full-bv-track study — times the exact BDD engine
/// and the native BMC/k-induction engine over every `.btor2` in `MUNUNU_HWMCC_DIR`,
/// emitting a `INHOUSE\t<file>\t<native_verdict>\t<native_ms>\t<exact_verdict>\t<exact_ms>`
/// line per benchmark for the orchestration script to merge with the btormc/Pono
/// timings. Both are in-process (Z3 / BDD), so this pass is fast; the subprocess
/// members are timed separately (their CLIs). `MUNUNU_NATIVE_MAXK` /
/// `MUNUNU_NATIVE_MS` override the native depth / per-check budget (default 100 /
/// 30000 ms). Verdicts: `sat` (reachable) / `unsat` (unreachable / safe) / `?`.
#[test]
#[ignore = "study runner: provide MUNUNU_HWMCC_DIR; driven by scripts/hwmcc_bv_study.sh"]
fn hwmcc_inhouse_timing() {
    use mununu_core::adapter::btor2::native_bmc::{SafetyVerdict, decide_bad_safety};
    use mununu_core::adapter::btor2::parser::parse as parse_btor2;
    use std::time::Instant;

    let Some(dir) = std::env::var_os("MUNUNU_HWMCC_DIR") else {
        eprintln!("MUNUNU_HWMCC_DIR unset — nothing to time.");
        return;
    };
    let max_k: u32 = std::env::var("MUNUNU_NATIVE_MAXK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let per_check_ms: u32 = std::env::var("MUNUNU_NATIVE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);

    let mut files: Vec<PathBuf> = std::fs::read_dir(PathBuf::from(&dir))
        .unwrap_or_else(|e| panic!("read dir: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("btor2") | Some("btor")
            )
        })
        .collect();
    files.sort();

    for path in &files {
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        // Exact (in-process BDD): Some(bool) reachability, or abstain.
        let t = Instant::now();
        let exact = exact_bad_reachable(&content).ok();
        let exact_ms = t.elapsed().as_millis();
        let exact_v = match exact {
            Some(true) => "sat",
            Some(false) => "unsat",
            None => "?",
        };
        // Native (in-process BMC + k-induction).
        let (native_v, native_ms) = match parse_btor2(&content) {
            Ok(file) => {
                let t = Instant::now();
                let v = decide_bad_safety(&file, max_k, Some(per_check_ms));
                let ms = t.elapsed().as_millis();
                let s = match v {
                    Ok(SafetyVerdict::Violated { .. }) => "sat",
                    Ok(SafetyVerdict::Safe { .. }) => "unsat",
                    _ => "?",
                };
                (s, ms)
            }
            Err(_) => ("?", 0),
        };
        println!("INHOUSE\t{fname}\t{native_v}\t{native_ms}\t{exact_v}\t{exact_ms}");
    }
}

// ============================================================================
// P2 — corpus differential over real open designs, with the MONOTONE VERDICT
// LEDGER invariant.
//
// The corpus (plan: `.claude/plans/differential-oracle-e2e-suite.md`) runs each
// real design's OWN SVA (extracted untouched from source) plus a mununu-exclusive
// liveness/recoverability annotation through `sv verify-auto`, and checks the
// per-property verdict against a recorded LEDGER under the refinement lattice:
//
//     ⊥ (Bottom / Skipped)  ⊑  { True, False }
//
//   - A recorded DEFINITE verdict (True/False) MUST be preserved exactly. A
//     True↔False flip is a SOUNDNESS bug; a definite→⊥ is a precision REGRESSION.
//   - A recorded ⊥ MAY stay ⊥ or FLIP UP to a definite verdict — that is an
//     IMPROVEMENT as our abstraction / verification refines (update the ledger to
//     lock the new definite value in). Verdicts only ever move UP the lattice.
//
// The corpus designs + their SVA are kept BYTE-FOR-BYTE from upstream (models-from-
// source, claims-integrity); only the `@mununu_guarantee` liveness annotation is
// added, and it is a distinct property, never a rewrite of the design's SVA.
// ============================================================================

/// A recorded verdict in the refinement lattice. `Indefinite` covers both `⊥`
/// (extracted, undecided) and `Skipped` (not yet extractable) — both may flip up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedgerVerdict {
    True,
    False,
    Indefinite,
}

fn outcome_verdict(o: &VerifyOutcome) -> LedgerVerdict {
    match o {
        VerifyOutcome::Holds => LedgerVerdict::True,
        VerifyOutcome::Violated { .. } => LedgerVerdict::False,
        VerifyOutcome::Unknown { .. } | VerifyOutcome::Skipped { .. } => LedgerVerdict::Indefinite,
    }
}

/// The monotone-refinement check. Returns `Some(new_definite)` when a recorded `⊥`
/// flipped UP to a definite verdict (an improvement to record in the ledger); panics
/// on any lattice violation (a definite verdict that flipped or regressed).
#[must_use]
fn assert_monotone(
    design: &str,
    property: &str,
    recorded: LedgerVerdict,
    current: LedgerVerdict,
) -> Option<LedgerVerdict> {
    match recorded {
        LedgerVerdict::True | LedgerVerdict::False => {
            assert_eq!(
                current, recorded,
                "MONOTONE VIOLATION: {design}.{property} recorded definite {recorded:?} but is now \
                 {current:?} — a definite verdict must be preserved exactly (a True↔False flip is a \
                 soundness bug; a definite→⊥ is a precision regression)."
            );
            None
        }
        LedgerVerdict::Indefinite => {
            if current != LedgerVerdict::Indefinite {
                // ⊥ → definite: an allowed improvement. Surface it so the ledger is updated.
                eprintln!(
                    "IMPROVEMENT: {design}.{property} ⊥ → {current:?} — the pipeline refined; \
                     update the ledger to lock this definite verdict in."
                );
                Some(current)
            } else {
                None
            }
        }
    }
}

/// One corpus design: its untouched sources (primary + deps), top module, any config
/// concretization, and the recorded ledger (property-name-substring → verdict). The
/// design's own SVA is verified automatically by `verify-auto`; `annotations` carries
/// the mununu-exclusive liveness properties prepended as `@mununu_guarantee` comments.
struct CorpusDesign {
    name: &'static str,
    /// Example directory under `examples/verify/`; every source lives in `<dir>/source/`.
    dir: &'static str,
    /// Source filenames in `<dir>/source/`, read UNTOUCHED from disk. `files[0]` is the
    /// primary DUT (gets the `@mununu_guarantee` annotations); the rest — the
    /// `prim_assert.sv` macro shim + the byte-exact upstream package/submodule closure —
    /// are the additional_sources fed to sv2v/yosys.
    files: &'static [&'static str],
    top: &'static str,
    /// `@mununu_guarantee <mu-formula>` lines prepended to the PRIMARY source.
    annotations: &'static [&'static str],
    config: &'static [(&'static str, u64)],
    /// (property-name-substring, recorded verdict) — the monotone ledger.
    ledger: &'static [(&'static str, LedgerVerdict)],
}

/// Run one corpus design through `verify-auto` (exact-symbolic, untouched sources + the
/// prepended `@mununu_guarantee` liveness annotations) and return the OBSERVED verdict for
/// every ledger property, in ledger order. No monotone check here — this is the raw
/// oracle read that both the strict ledger gate and the observation census share. Panics
/// only on a SETUP failure (verify_auto errored, or a ledger property is absent from the
/// report) — never on a verdict fact, so a definite/⊥ outcome is always returned, not raised.
/// Which verification engine to route the corpus property through. All three share the same
/// front-end (slang → sv2v → yosys → BTOR2 → reset-gated `$past`-shadow model) and the same
/// per-property μ-calculus; they differ only in how the abstract transition relation is
/// decided. Mirrors the CLI/API `--engine` selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    /// Default predicate-abstraction CEGAR (SMT all-pairs `cegar_refine_loop`): the current
    /// production default (`symbolic_engine=false, exact_symbolic=false`).
    Cegar,
    /// Predicate-cube BDD CEGAR loop (`--engine symbolic`, `symbolic_engine=true`): the
    /// default-flip *candidate*. Same cube space + init-cube read as `Cegar`; differs only in
    /// the refinement loop (`symbolic_cegar_refine`).
    SymbolicCube,
    /// Exact full-state ROBDD MC (`--engine exact-symbolic`, `exact_symbolic=true`): 2-valued,
    /// no ⊥, bounded to the bit cap (COI-pruned). The corpus ledger's oracle.
    ExactSymbolic,
    /// PORTFOLIO (sequential) — exact → symbolic → explicit, early-exit when all decided.
    PortfolioSequential,
    /// PORTFOLIO (parallel) — all three engines concurrently, merge, take any definite.
    PortfolioParallel,
}

impl Engine {
    /// Apply this engine's selection to a fresh option set.
    fn apply(self, opts: &mut VerifyAutoOptions) {
        let (sym, exact, portfolio) = match self {
            Engine::Cegar => (false, false, None),
            Engine::SymbolicCube => (true, false, None),
            Engine::ExactSymbolic => (false, true, None),
            Engine::PortfolioSequential => (false, false, Some(PortfolioMode::Sequential)),
            Engine::PortfolioParallel => (false, false, Some(PortfolioMode::Parallel)),
        };
        opts.symbolic_engine = sym;
        opts.exact_symbolic = exact;
        opts.portfolio = portfolio;
    }
}

/// Build the sources (untouched design + prepended `@mununu_guarantee` annotations) and run
/// `verify_auto` under `engine`, returning the FULL report (so a caller can inspect notes /
/// counterexamples, not just the ledger verdicts). Panics only on a SETUP failure.
fn corpus_report(
    d: &CorpusDesign,
    engine: Engine,
) -> mununu_core::adapter::slang::verify_auto::AutoVerifyReport {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
    let read = |file: &str| {
        let p = root.join(d.dir).join("source").join(file);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    };
    // Prepend the mununu-exclusive annotations to the primary source (source stays
    // untouched on disk; only this in-memory copy carries the added properties).
    let primary_name = d.files[0];
    let mut primary = String::new();
    for ann in d.annotations {
        primary.push_str("// @mununu_guarantee ");
        primary.push_str(ann);
        primary.push('\n');
    }
    primary.push_str(&read(primary_name));

    let mut sources = vec![(primary_name.to_string(), primary)];
    for file in &d.files[1..] {
        sources.push((file.to_string(), read(file)));
    }
    let yopts = YosysOptions {
        top: Some(d.top.to_string()),
        use_sv2v: true,
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    let config_values = d.config.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    // exact-symbolic (ROBDD, 2-valued, ≤40 bits) vs the two cube engines (abstraction-based,
    // no hard bit cap) vs the multi-engine portfolio — selected by `engine.apply`.
    let mut opts = VerifyAutoOptions {
        config_values,
        ..Default::default()
    };
    engine.apply(&mut opts);
    verify_auto(&sources, &yopts, &opts)
        .unwrap_or_else(|e| panic!("{}: verify_auto failed: {}", d.name, e.message))
}

fn run_corpus_verdicts(
    d: &CorpusDesign,
    engine: Engine,
) -> Vec<(&'static str, LedgerVerdict, String)> {
    let report = corpus_report(d, engine);
    d.ledger
        .iter()
        .map(|(needle, _)| {
            let prop = report
                .properties
                .iter()
                .find(|p| p.name.contains(needle) || p.formula.contains(needle))
                .unwrap_or_else(|| {
                    panic!(
                        "{}: ledger property `{needle}` not in the report; got {:?}",
                        d.name,
                        report
                            .properties
                            .iter()
                            .map(|p| &p.name)
                            .collect::<Vec<_>>()
                    )
                });
            (
                *needle,
                outcome_verdict(&prop.outcome),
                outcome_detail(&prop.outcome),
            )
        })
        .collect()
}

/// A one-line cause for a non-definite outcome (empty for a definite Holds/Violated) — the
/// "why is this ⊥" the census surfaces so the mitigation path is actionable.
fn outcome_detail(o: &VerifyOutcome) -> String {
    match o {
        VerifyOutcome::Holds | VerifyOutcome::Violated { .. } => String::new(),
        VerifyOutcome::Unknown { unknown_cells } => format!("Unknown ({unknown_cells} cells)"),
        VerifyOutcome::Skipped { reason } => format!("Skipped: {reason}"),
    }
}

/// Run one corpus design and enforce the monotone ledger on every recorded property.
/// Returns the list of ⊥→definite improvements (empty in steady state).
fn check_corpus_design(d: &CorpusDesign) -> Vec<(String, LedgerVerdict)> {
    let mut improvements = Vec::new();
    for ((needle, current, _), (_, recorded)) in run_corpus_verdicts(d, Engine::ExactSymbolic)
        .into_iter()
        .zip(d.ledger)
    {
        if let Some(up) = assert_monotone(d.name, needle, *recorded, current) {
            improvements.push((format!("{}.{needle}", d.name), up));
        }
    }
    improvements
}

/// The corpus. Grows by adding a `CorpusDesign` entry (untouched source + a ledger).
/// Seeded with uart_tx; the verification-prospector backlog (≥15 designs) feeds the
/// expansion. csrng's recoverability flip lives in the v8 example + its own e2e test.
const CORPUS: &[CorpusDesign] = &[
    CorpusDesign {
        name: "uart_tx",
        dir: "m1_opentitan_uart_tx",
        files: &["uart_tx.sv"],
        top: "uart_tx",
        annotations: &[
            // AG AF (a transmission always completes) — VIOLATED (a persistent write / stalled
            // tick holds the counter non-zero forever); AG EF (recoverability) — HOLDS.
            "nu X. ((mu Y. ((bit_cnt_q == 0) or [] Y)) and [] X)",
            "nu Y. ((mu X. ((bit_cnt_q == 0) or <> X)) and [] Y)",
        ],
        config: &[("rst_ni", 1)], // gate reset (see edn note) for a meaningful recoverability verdict
        ledger: &[
            // Recorded definite verdicts — must be preserved (a flip = a soundness/precision bug).
            ("(mu Y. ((bit_cnt_q == 0) or [] Y))", LedgerVerdict::False), // AG AF: VIOLATED
            ("(mu X. ((bit_cnt_q == 0) or <> X))", LedgerVerdict::True),  // AG EF: HOLDS
        ],
    },
    // ---- verification-prospector tranche 1 (OpenTitan Apache-2.0). Every `files` entry is
    // BYTE-EXACT upstream at commit 558921c (the DUT + its full package/submodule closure);
    // only `prim_assert.sv` is a local synthesis-macro shim. Enum symbols in the atoms are
    // folded to their integer values by the slang frontend's `resolve_enum_refs`. Ledgers
    // seeded `Indefinite` and calibrated to the OBSERVED exact-symbolic verdict — a definite
    // observation locks it (a later flip = a soundness/precision bug). ----
    //
    // edn_main_sm — 9-bit sparse FSM, csrng sibling. `AG EF Idle` recoverability. Multiple
    // terminal traps (Error via local_escalate_i, RejectCsrngEntropy via csrng_ack_err_i —
    // edn_main_sm.sv:179,188) make recovery VIOLATED even without escalation (unlike csrng,
    // whose only obstruction is local_escalate_i — see the csrng flip below).
    CorpusDesign {
        name: "edn_main_sm",
        dir: "dc_opentitan_edn_main_sm",
        files: &[
            "edn_main_sm.sv",
            "prim_assert.sv",
            "edn_pkg.sv",
            "entropy_src_pkg.sv",
            "prim_util_pkg.sv",
            "csrng_pkg.sv",
            "csrng_reg_pkg.sv",
        ],
        top: "edn_main_sm",
        // Idle = 9'b011000001 = 193 (enum symbols are NOT folded in the @mununu_guarantee
        // mu-formula path — only in the design's own SVA — so the atom uses the integer).
        annotations: &["nu Y. ((mu X. ((state_q == 193) or <> X)) and [] Y)"],
        // rst_ni pinned INACTIVE (active-low → 1). The prim_assert shim strips the design's
        // `disable iff (!rst_ni)` SVA that auto reset-gating relies on, so WITHOUT this pin
        // reset is a free input that trivially "recovers" to Idle (a reset-trivial False-True).
        // Pinning makes recoverability MEANINGFUL: recovery must happen via the FSM, not reset.
        // Verified: free escalate ⇒ VIOLATED (the Error trap, edn_main_sm.sv:179 is terminal);
        // EF(AG Error) confirms a permanently-stuck Error is reachable.
        config: &[("rst_ni", 1)],
        ledger: &[("(state_q == 193)", LedgerVerdict::False)], // VIOLATED (Error/Reject traps)
    },
    // prim_esc_receiver — single-clock 5-state escalation FSM ({Idle,Check,PingResp,EscResp,
    // SigInt}). `AG EF Idle` recoverability.
    CorpusDesign {
        name: "prim_esc_receiver",
        dir: "dc_opentitan_prim_esc_receiver",
        // prim_count.sv (the timeout counter submodule) is vendored too, else it stays a
        // chaotic blackbox that unsoundly frees the timeout→esc_req path.
        files: &[
            "prim_esc_receiver.sv",
            "prim_assert.sv",
            "prim_esc_pkg.sv",
            "prim_count.sv",
            "prim_count_pkg.sv",
            "prim_util_pkg.sv",
        ],
        top: "prim_esc_receiver",
        annotations: &["nu Y. ((mu X. ((state_q == 0) or <> X)) and [] Y)"], // Idle = 0
        config: &[("rst_ni", 1)], // gate reset (see edn note) — else recovery is reset-trivial
        ledger: &[("(state_q == 0)", LedgerVerdict::True)], // HOLDS via COI (recovers to Idle)
    },
    // aes_ctr_fsm — 3-state AES counter-mode FSM (reg `aes_ctr_cs`; CTR_IDLE/CTR_INCR/
    // CTR_ERROR). `AG EF CTR_IDLE` recoverability.
    CorpusDesign {
        name: "aes_ctr_fsm",
        dir: "dc_opentitan_aes_ctr_fsm",
        files: &[
            "aes_ctr_fsm.sv",
            "prim_assert.sv",
            "aes_pkg.sv",
            "aes_reg_pkg.sv",
            "prim_util_pkg.sv",
        ],
        top: "aes_ctr_fsm",
        annotations: &["nu Y. ((mu X. ((aes_ctr_cs == 14) or <> X)) and [] Y)"], // CTR_IDLE = 5'b01110 = 14
        config: &[("rst_ni", 1)], // gate reset (see edn note) — else recovery is reset-trivial
        ledger: &[("(aes_ctr_cs == 14)", LedgerVerdict::False)], // VIOLATED (CTR_ERROR trap)
    },
    // rom_ctrl_fsm — 8-state ROM-integrity FSM (mubi-tagged enum; `Done` = {6'b100000,
    // MuBi4True=4'h6} = 518). `AG EF Done` completion-recoverability. The full design is 834
    // register+input bits, but R-F5.6 cone-of-influence restricts the exact engine to the FSM
    // cone → VIOLATED (the glitch-reachable Invalid trap is terminal). Was a bit-cap ⊥ pre-COI.
    CorpusDesign {
        name: "rom_ctrl_fsm",
        dir: "dc_opentitan_rom_ctrl_fsm",
        files: &[
            "rom_ctrl_fsm.sv",
            "prim_assert.sv",
            "rom_ctrl_pkg.sv",
            "prim_mubi_pkg.sv",
            "prim_util_pkg.sv",
        ],
        top: "rom_ctrl_fsm",
        annotations: &["nu Y. ((mu X. ((state_q == 518) or <> X)) and [] Y)"], // Done = 518
        config: &[],
        ledger: &[("(state_q == 518)", LedgerVerdict::False)], // VIOLATED via COI (Invalid trap)
    },
    // prim_count — dual-redundant hardened counter (primary `cnt_o`, ResetValue 0).
    // `AG EF (cnt_o == 0)` clear-recoverability via `clr_i`.
    CorpusDesign {
        name: "prim_count",
        dir: "dc_opentitan_prim_count",
        files: &["prim_count.sv", "prim_assert.sv", "prim_count_pkg.sv"],
        top: "prim_count",
        annotations: &["nu Y. ((mu X. ((cnt_o == 0) or <> X)) and [] Y)"],
        config: &[("rst_ni", 1)], // gate reset (see edn note) — else recovery is reset-trivial
        ledger: &[("(cnt_o == 0)", LedgerVerdict::True)], // HOLDS (clearable via clr_i)
    },
    // ---- tranche 2 (OpenTitan Apache-2.0, byte-exact upstream at 558921c). All reset-gated
    // (rst_ni=1). R-F5.6 cone-of-influence restricts the exact engine to each property's cone,
    // so the large control FSMs (usbdev/aes_cipher/otbn) DECIDE (they were bit-cap ⊥ pre-COI). ----
    // prim_arbiter_ppc — round-robin arbiter; the `mask` register is its only state (resets 0).
    // AG EF (gnt_o==0): the arbiter can always return to no-grant (the priority `mask` register
    // is optimized away by yosys, so the atom binds to the combinational output `gnt_o`).
    CorpusDesign {
        name: "prim_arbiter_ppc",
        dir: "dc_opentitan_prim_arbiter_ppc",
        files: &["prim_arbiter_ppc.sv", "prim_assert.sv", "prim_util_pkg.sv"],
        top: "prim_arbiter_ppc",
        annotations: &["nu Y. ((mu X. ((gnt_o == 0) or <> X)) and [] Y)"],
        config: &[("rst_ni", 1)],
        ledger: &[("(gnt_o == 0)", LedgerVerdict::True)],
    },
    // prim_arbiter_tree — round-robin arbiter. AG EF (gnt_o==0) no-grant recoverability
    // (combinational output; the priority register does not survive synthesis by that name).
    CorpusDesign {
        name: "prim_arbiter_tree",
        dir: "dc_opentitan_prim_arbiter_tree",
        files: &["prim_arbiter_tree.sv", "prim_assert.sv"],
        top: "prim_arbiter_tree",
        annotations: &["nu Y. ((mu X. ((gnt_o == 0) or <> X)) and [] Y)"],
        config: &[("rst_ni", 1)],
        ledger: &[("(gnt_o == 0)", LedgerVerdict::True)],
    },
    // prim_packer_fifo — AG EF (depth_o==0): the packer FIFO is always drainable to empty.
    // HOLDS once the bit-blaster supports Mul + shifts (the pointer datapath); depth_o is a
    // registered output here (unlike prim_fifo_sync, where it is combinational).
    CorpusDesign {
        name: "prim_packer_fifo",
        dir: "dc_opentitan_prim_packer_fifo",
        files: &["prim_packer_fifo.sv", "prim_assert.sv"],
        top: "prim_packer_fifo",
        annotations: &["nu Y. ((mu X. ((depth_o == 0) or <> X)) and [] Y)"],
        config: &[("rst_ni", 1)],
        ledger: &[("(depth_o == 0)", LedgerVerdict::True)], // HOLDS (always drainable) via Mul/shift support
    },
    // prim_esc_sender — 5-state escalation-sender FSM (Idle=0). AG EF Idle recoverability.
    CorpusDesign {
        name: "prim_esc_sender",
        dir: "dc_opentitan_prim_esc_sender",
        files: &["prim_esc_sender.sv", "prim_assert.sv", "prim_esc_pkg.sv"],
        top: "prim_esc_sender",
        annotations: &["nu Y. ((mu X. ((state_q == 0) or <> X)) and [] Y)"], // Idle = 0
        config: &[("rst_ni", 1)],
        ledger: &[("(state_q == 0)", LedgerVerdict::True)], // HOLDS (esc_sender recovers to Idle)
    },
    // prim_alert_sender — 7-state alert-sender FSM (Idle=0), the alert-path sibling of
    // prim_esc_sender. Instantiated prim_diff_decode / prim_sec_anchor_* submodules are
    // blackboxed (chaotic-stub), so the closure is DUT + pkg + the prim_assert shim.
    // AG EF Idle recoverability is VIOLATED (unlike esc_sender, which HOLDS): with the
    // blackboxed submodules a free alert/ping/sigint environment can drive the FSM into a
    // reachable state with no path back to Idle — an in-model expected violation (an
    // adversarial never-acking environment traps it out of Idle), analogous to the other
    // corpus Violated designs. Non-spurious: reset is gated (rst_ni=1) and Idle=0 is the
    // real idle state, not a vacuous atom.
    CorpusDesign {
        name: "prim_alert_sender",
        dir: "dc_opentitan_prim_alert_sender",
        files: &[
            "prim_alert_sender.sv",
            "prim_assert.sv",
            "prim_alert_pkg.sv",
        ],
        top: "prim_alert_sender",
        annotations: &["nu Y. ((mu X. ((state_q == 0) or <> X)) and [] Y)"], // Idle = 0
        config: &[("rst_ni", 1)],
        ledger: &[("(state_q == 0)", LedgerVerdict::False)], // VIOLATED (confirmed in docker)
    },
    // usbdev_linkstate — 6-state USB link FSM (LinkDisconnected=0). AG EF LinkDisconnected —
    // HOLDS via COI (the link is always disconnectable; the 12-bit timers are pruned).
    CorpusDesign {
        name: "usbdev_linkstate",
        dir: "dc_opentitan_usbdev_linkstate",
        files: &["usbdev_linkstate.sv", "prim_assert.sv"],
        top: "usbdev_linkstate",
        annotations: &["nu Y. ((mu X. ((link_state_q == 0) or <> X)) and [] Y)"], // LinkDisconnected = 0
        config: &[("rst_ni", 1)],
        ledger: &[("(link_state_q == 0)", LedgerVerdict::True)], // HOLDS via COI (link always disconnectable)
    },
    // aes_cipher_control_fsm — 7-state AES cipher-core FSM (CIPHER_CTRL_IDLE=6'b001001=9).
    // AG EF CIPHER_CTRL_IDLE — VIOLATED via COI (the aes_reg_pkg datapath is pruned).
    CorpusDesign {
        name: "aes_cipher_control_fsm",
        dir: "dc_opentitan_aes_cipher_control_fsm",
        files: &[
            "aes_cipher_control_fsm.sv",
            "prim_assert.sv",
            "aes_pkg.sv",
            "aes_reg_pkg.sv",
            "prim_util_pkg.sv",
        ],
        top: "aes_cipher_control_fsm",
        annotations: &["nu Y. ((mu X. ((aes_cipher_ctrl_cs == 9) or <> X)) and [] Y)"], // CIPHER_CTRL_IDLE = 9
        config: &[("rst_ni", 1)],
        ledger: &[("(aes_cipher_ctrl_cs == 9)", LedgerVerdict::False)], // VIOLATED via COI (cipher error trap)
    },
    // otbn_start_stop_control — OTBN secure-wipe start/stop FSM. Big closure (lc_ctrl/otp/secded)
    CorpusDesign {
        name: "otbn_start_stop_control",
        dir: "dc_opentitan_otbn_start_stop_control",
        files: &[
            "otbn_start_stop_control.sv",
            "prim_assert.sv",
            "otbn_pkg.sv",
            "otp_ctrl_pkg.sv",
            "lc_ctrl_pkg.sv",
            "lc_ctrl_reg_pkg.sv",
            "lc_ctrl_state_pkg.sv",
            "prim_mubi_pkg.sv",
            "prim_secded_pkg.sv",
            "prim_trivium_pkg.sv",
            "prim_util_pkg.sv",
        ],
        top: "otbn_start_stop_control",
        // Halt = 8'b00000001 = 1 (the idle/done state; reset state is Initial=167). AG EF Halt
        // = the secure-wipe FSM always returns to idle.
        annotations: &["nu Y. ((mu X. ((state_q == 1) or <> X)) and [] Y)"],
        config: &[("rst_ni", 1)],
        ledger: &[("(state_q == 1)", LedgerVerdict::False)], // VIOLATED via COI (Locked trap)
    },
    // csrng_main_sm — sparse command FSM (MainSmIdle = 6'b110111 = 55). AG EF Idle
    // recoverability, VIOLATED: beyond local_escalate_i, an unsupported command drives the FSM
    // to the terminal MainSmError trap (csrng_main_sm.sv:122 `default: state_d = MainSmError`),
    // so with the command inputs free, recovery fails. The CLEAN escalation flip (escalate-free
    // ⇒ VIOLATED, local_escalate_i=0 ⇒ HOLDS) needs the input-concretized setup of the shipped
    // v8_csrng_escalation_recoverability example (which pins acmd_i/flag0_i); here the design is
    // exercised with all command inputs free — the honest unconstrained recoverability verdict.
    CorpusDesign {
        name: "csrng_main_sm",
        dir: "dc_opentitan_csrng_main_sm",
        files: &[
            "csrng_main_sm.sv",
            "prim_assert.sv",
            "csrng_pkg.sv",
            "csrng_reg_pkg.sv",
            "entropy_src_pkg.sv",
            "prim_util_pkg.sv",
        ],
        top: "csrng_main_sm",
        annotations: &["nu Y. ((mu X. ((state_q == 55) or <> X)) and [] Y)"], // MainSmIdle = 55
        config: &[("rst_ni", 1)],
        ledger: &[("(state_q == 55)", LedgerVerdict::False)], // VIOLATED (MainSmError trap, inputs free)
    },
    // prim_fifo_sync — synchronous FIFO; AG EF (depth_o==0) drainability = HOLDS. `depth_o` is
    // COMBINATIONAL here (from the wptr/rptr, not a registered output like prim_packer_fifo's),
    // so it binds via the exact engine's named-combinational-signal support; its cone is just the
    // pointers (the data storage is out of it). prim_fifo_assert.svh is a `include`d
    // header (staged so the include resolves, never compiled as a top-level source).
    CorpusDesign {
        name: "prim_fifo_sync",
        dir: "dc_opentitan_prim_fifo_sync",
        files: &[
            "prim_fifo_sync.sv",
            "prim_assert.sv",
            "prim_fifo_assert.svh",
            "prim_util_pkg.sv",
        ],
        top: "prim_fifo_sync",
        annotations: &["nu Y. ((mu X. ((depth_o == 0) or <> X)) and [] Y)"],
        config: &[("rst_ni", 1)],
        ledger: &[("(depth_o == 0)", LedgerVerdict::True)],
    },
    // ibex_controller — the lowRISC ibex RISC-V core's main control FSM (ctrl_fsm_e, 4-bit:
    // RESET=0 … DECODE=5 … DBG_TAKEN_ID=9). Byte-exact from lowRISC/ibex c6edaa40; pure control
    // logic (no submodules), self-contained ibex_pkg (791 lines) + prim_assert + a dv_fcov synth
    // stub. The first NON-prim, CPU-scale design in the corpus. AG EF DECODE recoverability: does
    // the core always return to executing instructions? (`ctrl_fsm_cs == 5` = DECODE).
    //
    // The EXACT engine decides HOLDS (True) — the core always returns to DECODE. The cube engines
    // return an HONEST ⊥: with the sound `SmtAllPairs` may-relation (AR-S2 retired the sampling-may
    // default + its A.4 ⊥-guard), the 1-predicate cube abstraction genuinely evaluates the νμ
    // recoverability to KleeneBot — too coarse to decide, but never a spurious definite. So
    // exact=True, cube=⊥, no cross-engine contradiction (soundness-flips=0). Before AR-S2 the cube
    // would have produced a spurious VIOLATED that the A.4 stopgap downgraded to ⊥; the sound may
    // makes that stopgap unnecessary.
    CorpusDesign {
        name: "ibex_controller",
        dir: "dc_lowrisc_ibex_controller",
        files: &[
            "ibex_controller.sv",
            "prim_assert.sv",
            "dv_fcov_macros.svh", // coverage-macro synth stub (ibex_controller includes it)
            "ibex_pkg.sv",
        ],
        top: "ibex_controller",
        annotations: &["nu Y. ((mu X. ((ctrl_fsm_cs == 5) or <> X)) and [] Y)"], // DECODE = 5
        config: &[("rst_ni", 1)],
        // HOLDS (exact engine) — the core always returns to executing. Non-spurious (reset gated,
        // DECODE=5 is the real execution state). The cube engines honestly ⊥ (sound-may KleeneBot).
        ledger: &[("(ctrl_fsm_cs == 5)", LedgerVerdict::True)],
    },
    // keymgr_ctrl — OpenTitan key-manager control FSM (sparse `state_e`, 10-bit: StCtrlReset =
    // 0b1101100001 = 865 … StCtrlDisabled/Invalid terminal traps). Byte-exact from OpenTitan
    // 558921c; a 10-package closure (keymgr + otp_ctrl + lc_ctrl + prim mubi/secded/util) with
    // the submodules (keymgr_op_state_ctrl / keymgr_err / prim_count / prim_mubi4_sender /
    // prim_secded_inv_72_64_dec) blackboxed. AG EF StCtrlReset recoverability: can it always get
    // back to reset? (`state_q == 865`). Ledger `Indefinite` = probe placeholder → lock in docker.
    CorpusDesign {
        name: "keymgr_ctrl",
        dir: "dc_opentitan_keymgr_ctrl",
        files: &[
            "keymgr_ctrl.sv",
            "prim_assert.sv",
            "dv_fcov_macros.svh",
            "prim_util_pkg.sv",
            "prim_secded_pkg.sv",
            "prim_mubi_pkg.sv",
            "lc_ctrl_state_pkg.sv",
            "lc_ctrl_reg_pkg.sv",
            "lc_ctrl_pkg.sv",
            "otp_ctrl_pkg.sv",
            "keymgr_reg_pkg.sv",
            "keymgr_pkg.sv",
        ],
        top: "keymgr_ctrl",
        annotations: &["nu Y. ((mu X. ((state_q == 865) or <> X)) and [] Y)"], // StCtrlReset = 865
        config: &[("rst_ni", 1)],
        // VIOLATED (exact) — the key manager does NOT return to StCtrlReset: its terminal
        // StCtrlDisabled / StCtrlInvalid hardening states only reset escapes (the same
        // SEC_CM sparse-FSM pattern as csrng/edn — an EXPECTED violation, not a finding).
        // Non-spurious: reset gated (rst_ni=1), StCtrlReset=865 is the real reset state.
        // Decidable only after the > 128-bit binary-constant bit-blast fix (256-bit key-state).
        ledger: &[("(state_q == 865)", LedgerVerdict::False)],
    },
];

#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn diff_corpus_monotone_verdict_ledger() {
    let mut all_improvements = Vec::new();
    for d in CORPUS {
        all_improvements.extend(check_corpus_design(d));
    }
    // Improvements (⊥→definite) are NOT failures — they are the signal to update the
    // ledger. Print them; the suite stays green. A definite flip/regress already panicked.
    if !all_improvements.is_empty() {
        eprintln!("\n=== ledger improvements (⊥→definite) — update the ledger ===");
        for (prop, v) in &all_improvements {
            eprintln!("  {prop}: {v:?}");
        }
    }
}

/// One catch_unwind wrapper around a corpus run under a given engine.
fn observe_engine(
    d: &CorpusDesign,
    engine: Engine,
) -> Result<Vec<(&'static str, LedgerVerdict, String)>, String> {
    std::panic::catch_unwind(|| run_corpus_verdicts(d, engine)).map_err(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "<non-string panic>".to_string())
    })
}

/// Verdict CENSUS over the whole corpus — a non-failing diagnostic that runs every design
/// under `catch_unwind`, so one docker pass surfaces EVERY design's observed verdict (or its
/// setup error) without one failure aborting the batch. Each design runs under the exact
/// engine first; any property the exact engine leaves ⊥ (its 40-bit cap) is retried under the
/// EXPLICIT predicate-cube CEGAR engine — the mitigation the exact engine's own Skip message
/// recommends — and the fallback verdict is shown. Prints per-design T/F/⊥ lines plus a tally
/// for each engine, the raw input for the "how many T/F/⊥, why the ⊥s, and the path forward"
/// census. Never asserts (a wrong verdict is caught by the strict ledger gate above).
#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn diff_corpus_verdict_census() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    // The explicit-engine ⊥-fallback is OPT-IN (`MUNUNU_CENSUS_EXPLICIT=1`): the explicit CEGAR
    // on the big closures (otbn) can run tens of minutes, so the default census is exact-only
    // and fast. Set the env var for the full "path forward" comparison.
    let want_explicit = std::env::var_os("MUNUNU_CENSUS_EXPLICIT").is_some();

    let (mut t, mut f, mut bot, mut err) = (0u32, 0u32, 0u32, 0u32);
    // How many exact-engine ⊥s the explicit engine rescued to a definite verdict.
    let (mut rescued, mut still_bot) = (0u32, 0u32);
    eprintln!(
        "\n================ corpus verdict census (exact engine; explicit fallback for ⊥) ================"
    );
    for d in CORPUS {
        let exact = observe_engine(d, Engine::ExactSymbolic);
        // Only pay for the explicit pass if opted in AND the exact pass left something ⊥.
        let needs_fallback = want_explicit
            && matches!(&exact, Ok(props) if props.iter().any(|(_, v, _)| *v == LedgerVerdict::Indefinite));
        let explicit = if needs_fallback {
            Some(observe_engine(d, Engine::Cegar))
        } else {
            None
        };

        match &exact {
            Ok(props) => {
                for (i, (needle, v, detail)) in props.iter().enumerate() {
                    match v {
                        LedgerVerdict::True => t += 1,
                        LedgerVerdict::False => f += 1,
                        LedgerVerdict::Indefinite => bot += 1,
                    }
                    // For a ⊥, look up the explicit-engine fallback verdict for the same property.
                    let fb = if *v == LedgerVerdict::Indefinite {
                        match &explicit {
                            Some(Ok(ex)) => match ex.get(i) {
                                Some((_, ev, _)) if *ev != LedgerVerdict::Indefinite => {
                                    rescued += 1;
                                    format!("  [explicit ⇒ {ev:?}]")
                                }
                                Some((_, _, edetail)) => {
                                    still_bot += 1;
                                    format!("  [explicit ⇒ ⊥: {edetail}]")
                                }
                                None => String::new(),
                            },
                            Some(Err(e)) => {
                                still_bot += 1;
                                format!("  [explicit ⇒ ERROR: {}]", e.lines().next().unwrap_or(""))
                            }
                            None => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    eprintln!("  {:32} {needle:34} -> {v:?}  {detail}{fb}", d.name);
                }
            }
            Err(msg) => {
                err += 1;
                let first = msg.lines().next().unwrap_or(msg);
                eprintln!("  {:32} {:34} -> ERROR: {first}", d.name, "");
            }
        }
    }
    std::panic::set_hook(prev);
    eprintln!(
        "---------------------------------------------------------------------------------------------"
    );
    eprintln!("  exact:  True={t}  False={f}  Indefinite(⊥)={bot}  setup-error={err}");
    eprintln!(
        "  explicit fallback on the {bot} exact-⊥:  rescued→definite={rescued}  still-⊥={still_bot}"
    );
    eprintln!(
        "=============================================================================================\n"
    );
}

/// The DEFAULT-FLIP soundness + precision differential (roadmap §"Default-flip gate"). Runs
/// every corpus property through BOTH cube engines — the current production default `Cegar`
/// (predicate-abstraction SMT all-pairs) and the flip *candidate* `SymbolicCube` (predicate-cube
/// BDD CEGAR) — and cross-checks each against the recorded EXACT ORACLE (`d.ledger`, the
/// full-state ROBDD MC, all-definite on the corpus). Three anchors:
///
///   1. **cube-vs-exact-oracle (HARD GATE)** — any cube-engine DEFINITE verdict that disagrees
///      with the exact oracle is a soundness bug: the cube abstractions must refine toward the
///      full-state MC, never contradict it. The strongest anchor — catches a single wrong-definite
///      that a cross-cube check would miss if BOTH cubes agreed on the wrong answer.
///   2. **cross-cube flip (HARD GATE)** — the two cube engines returning OPPOSITE definites is a
///      bug in one of them. Collected across all designs, asserted at the end (a setup error in
///      one design can't mask a flip in another).
///   3. **precision delta (REPORTED)** — def↔⊥ in either direction: `Cegar` definite / `Sym` ⊥ is
///      a would-be regression; the reverse is a would-be gain.
///
/// **Empirical finding (2026-07-06, fast-FSM subset):** the two cube engines are COMPLEMENTARY,
/// not dominated — `SymbolicCube` decides several liveness properties `Cegar` leaves ⊥ (uart_tx
/// AG-AF / AG-EF, prim_count) while `Cegar` decides one `SymbolicCube` leaves ⊥ (aes_ctr_fsm).
/// Both agree with the exact oracle wherever definite. So a blind default-SWAP is NOT justified
/// (it trades regressions for gains); the sound production choice is a PORTFOLIO — take the
/// definite verdict from whichever engine produces one (exact preferred; cube fallback), which
/// this differential proves sound (no cross-engine contradiction, no oracle violation).
///
/// Each design runs under `catch_unwind`; a setup error is reported, not fatal. `MUNUNU_PARITY_ONLY`
/// (comma-separated design names) restricts the run to a subset — the fast FSMs for a quick read;
/// unset runs the whole corpus (slow: the big closures cost minutes per engine).
#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn diff_corpus_cegar_vs_symbolic_engine_parity() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let only: Option<Vec<String>> = std::env::var("MUNUNU_PARITY_ONLY")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let selected = |name: &str| {
        only.as_ref()
            .is_none_or(|list| list.iter().any(|n| n == name))
    };

    // Collected across all designs so no single design's setup error masks a violation elsewhere.
    // A cross-cube flip (Cegar↔Sym opposite definite) OR a disagreement with the exact oracle
    // (`d.ledger`, all-definite on the corpus) is a genuine soundness bug.
    let mut soundness_flips: Vec<String> = Vec::new();
    let mut oracle_violations: Vec<String> = Vec::new();
    let (mut parity, mut regress, mut gain, mut both_bot, mut err) = (0u32, 0u32, 0u32, 0u32, 0u32);
    // Per-engine precision vs the exact oracle: how many corpus properties each cube engine
    // decides DEFINITELY (and every such decision agrees with the oracle — asserted below).
    let (mut cegar_definite, mut sym_definite, mut total_props) = (0u32, 0u32, 0u32);

    eprintln!(
        "\n===== cegar↔symbolic soundness + precision vs the exact oracle (default-flip evidence) ====="
    );
    for d in CORPUS.iter().filter(|d| selected(d.name)) {
        let cegar = observe_engine(d, Engine::Cegar);
        let symbolic = observe_engine(d, Engine::SymbolicCube);
        match (&cegar, &symbolic) {
            (Ok(cv), Ok(sv)) => {
                // Zip the two cube reads with the recorded exact-oracle ledger (same order).
                for (((needle, c, cdetail), (_, s, sdetail)), (_, oracle)) in
                    cv.iter().zip(sv.iter()).zip(d.ledger.iter())
                {
                    use LedgerVerdict::{False, Indefinite, True};
                    total_props += 1;
                    if *c != Indefinite {
                        cegar_definite += 1;
                    }
                    if *s != Indefinite {
                        sym_definite += 1;
                    }
                    // Soundness anchor 1 — each cube-engine DEFINITE verdict must equal the exact
                    // oracle's definite verdict (the oracle is 2-valued/all-definite on the corpus).
                    for (eng, v) in [("Cegar", c), ("SymbolicCube", s)] {
                        if *v != Indefinite && *v != *oracle {
                            oracle_violations.push(format!(
                                "{}.{needle}: {eng}={v:?} but exact oracle={oracle:?}",
                                d.name
                            ));
                        }
                    }
                    // Soundness anchor 2 — the two cube engines must not give OPPOSITE definites.
                    let tag = match (c, s) {
                        (True, True) | (False, False) => {
                            parity += 1;
                            "parity ✓"
                        }
                        (True, False) | (False, True) => {
                            soundness_flips.push(format!(
                                "{}.{needle}: Cegar={c:?} SymbolicCube={s:?}",
                                d.name
                            ));
                            "SOUNDNESS FLIP ✗"
                        }
                        (True | False, Indefinite) => {
                            regress += 1;
                            "regress (def→⊥)"
                        }
                        (Indefinite, True | False) => {
                            gain += 1;
                            "gain (⊥→def)"
                        }
                        (Indefinite, Indefinite) => {
                            both_bot += 1;
                            "both ⊥"
                        }
                    };
                    let detail = if s == &Indefinite {
                        format!("  [sym: {sdetail}]")
                    } else if c == &Indefinite {
                        format!("  [cegar: {cdetail}]")
                    } else {
                        String::new()
                    };
                    eprintln!(
                        "  {:28} {needle:32} Cegar={c:?} Sym={s:?} oracle={oracle:?}  {tag}{detail}",
                        d.name
                    );
                }
            }
            (c, s) => {
                err += 1;
                let msg = c
                    .as_ref()
                    .err()
                    .or(s.as_ref().err())
                    .map(String::as_str)
                    .unwrap_or("");
                eprintln!(
                    "  {:28} {:32} -> SETUP ERROR: {}",
                    d.name,
                    "",
                    msg.lines().next().unwrap_or("")
                );
            }
        }
    }
    std::panic::set_hook(prev);
    eprintln!(
        "-----------------------------------------------------------------------------------------------"
    );
    eprintln!(
        "  cross-cube: parity={parity}  soundness-flips={}  regress(def→⊥)={regress}  gain(⊥→def)={gain}  both-⊥={both_bot}  setup-error={err}",
        soundness_flips.len()
    );
    eprintln!(
        "  precision vs exact oracle ({total_props} props):  Cegar decides {cegar_definite}  SymbolicCube decides {sym_definite}  (oracle-violations={})",
        oracle_violations.len()
    );
    eprintln!(
        "  FLIP READ: neither cube engine dominates — a def↔⊥ split in BOTH directions means the sound production choice is a PORTFOLIO (exact ⊕ cube), not a blind default-swap.",
    );
    eprintln!(
        "===============================================================================================\n"
    );

    // Hard gate 1: a cross-cube True↔False disagreement is a genuine soundness bug in one of the
    // two cube engines — never acceptable, regardless of the flip decision.
    assert!(
        soundness_flips.is_empty(),
        "CEGAR↔SYMBOLIC SOUNDNESS FLIP(S): {soundness_flips:?} — the two cube engines returned \
         OPPOSITE definite verdicts on the same property; one is unsound."
    );
    // Hard gate 2: any cube-engine DEFINITE verdict that disagrees with the exact oracle is a
    // soundness bug — the exact full-state MC is the gold reference the cube abstractions must
    // refine toward, never contradict. This is the strongest of the three anchors: it catches a
    // single-engine wrong-definite that a cross-cube check would miss if BOTH cubes were wrong.
    assert!(
        oracle_violations.is_empty(),
        "CUBE-vs-EXACT-ORACLE SOUNDNESS VIOLATION(S): {oracle_violations:?} — a cube engine \
         returned a DEFINITE verdict contradicting the exact full-state model checker."
    );
}

/// The PORTFOLIO end-to-end gate. Runs uart_tx (both liveness properties, definite under the
/// exact engine) through the real `verify_auto → portfolio → merge` chain — validating the full
/// integration the hermetic combiner tests can't: the engine dispatch, the scoped-thread
/// parallel orchestration, and the sequential early-exit, all against live slang/sv2v/yosys +
/// the three real engines. Asserts:
///   1. `Cegar` alone leaves AT LEAST ONE of the two properties ⊥ — the gap the portfolio
///      closes. When this test was written it left both ⊥; by 2026-09 the engine decided the
///      diamond form (`mu X. ((bit_cnt_q == 0) or <> X)` → True) on its own and only the box
///      form was still ⊥ (mununu#562), so the precondition is stated as the gap that is
///      actually there and PRINTS it. If `Cegar` ever decides both, this fails loudly: the
///      gap is gone and the test must move to a design that still has one, not be weakened.
///   2. Both portfolio modes DECIDE both properties, matching the exact oracle (AG-AF=Violated,
///      AG-EF=Holds) — the portfolio recovers what the default engine misses.
///   3. Sequential and Parallel agree exactly (same engines, same merge).
///   4. No `portfolio-soundness-alarm` note fired (the runtime guard stayed quiet) and a
///      `portfolio` provenance note is present.
#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn e2e_portfolio_decides_what_the_default_engine_misses() {
    let uart = &CORPUS[0];
    assert_eq!(
        uart.name, "uart_tx",
        "expected uart_tx as the first corpus design"
    );

    // 1. Baseline: the default Cegar engine leaves at least one uart_tx liveness prop ⊥.
    let cegar = run_corpus_verdicts(uart, Engine::Cegar);
    let still_bottom: Vec<&str> = cegar
        .iter()
        .filter(|(_, v, _)| *v == LedgerVerdict::Indefinite)
        .map(|(n, _, _)| *n)
        .collect();
    eprintln!(
        "Cegar alone leaves ⊥ on uart_tx: {still_bottom:?} (of {} properties)",
        cegar.len()
    );
    assert!(
        !still_bottom.is_empty(),
        "precondition: Cegar must leave at least one uart_tx property ⊥ (the gap the portfolio \
         closes) — if it decides both, this gate has no gap left to test on this design and \
         must move to one that has; got {cegar:?}"
    );

    // 2 + 3. Both portfolio modes decide both properties, and agree with each other.
    let seq = run_corpus_verdicts(uart, Engine::PortfolioSequential);
    let par = run_corpus_verdicts(uart, Engine::PortfolioParallel);
    let seq_v: Vec<_> = seq.iter().map(|(n, v, _)| (*n, *v)).collect();
    let par_v: Vec<_> = par.iter().map(|(n, v, _)| (*n, *v)).collect();
    assert_eq!(
        seq_v, par_v,
        "portfolio-sequential and portfolio-parallel must return identical verdicts"
    );
    // The exact oracle: AG-AF (bit_cnt_q stalls) = Violated; AG-EF (recoverable) = Holds.
    let expected = [
        ("(mu Y. ((bit_cnt_q == 0) or [] Y))", LedgerVerdict::False),
        ("(mu X. ((bit_cnt_q == 0) or <> X))", LedgerVerdict::True),
    ];
    for (needle, want) in expected {
        let got = seq_v
            .iter()
            .find(|(n, _)| *n == needle)
            .unwrap_or_else(|| panic!("portfolio result missing `{needle}`: {seq_v:?}"));
        assert_eq!(
            got.1, want,
            "portfolio must decide `{needle}` as {want:?} (the exact-oracle verdict)"
        );
    }

    // 4. The runtime soundness guard stayed quiet; the provenance note is present.
    let report = corpus_report(uart, Engine::PortfolioParallel);
    assert!(
        !report
            .notes
            .iter()
            .any(|n| n.kind == "portfolio-soundness-alarm"),
        "no engine may contradict another (the soundness-alarm note must be absent)"
    );
    assert!(
        report.notes.iter().any(|n| n.kind == "portfolio"),
        "the portfolio provenance note (engines ran + decided-by tally) must be present"
    );
}

/// Diagnostic — dump the post-synthesis STATE-register + INPUT names of the atom-binding-⊥
/// designs (arbiter_ppc/tree, fifo_sync). Their corpus atoms (`mask`, `prio_mask_q`, `depth_o`)
/// bind to no state register; this prints the names yosys actually emits so the atoms can be
/// re-pointed. Non-failing; run when refining those atoms.
#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn dump_state_registers_atom_binding_todo() {
    use mununu_core::adapter::btor2::parser::{collect_symbols, parse as parse_btor2};
    use mununu_core::adapter::yosys::sv_to_btor2;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
    for name in ["prim_arbiter_ppc", "prim_arbiter_tree", "prim_fifo_sync"] {
        let d = CORPUS
            .iter()
            .find(|d| d.name == name)
            .expect("design in corpus");
        let read = |f: &str| {
            std::fs::read_to_string(root.join(d.dir).join("source").join(f))
                .unwrap_or_else(|e| panic!("read {f}: {e}"))
        };
        let primary = read(d.files[0]);
        let additional: Vec<(String, String)> = d.files[1..]
            .iter()
            .map(|f| (f.to_string(), read(f)))
            .collect();
        let yopts = YosysOptions {
            top: Some(d.top.to_string()),
            use_sv2v: true,
            additional_sources: additional,
            ..Default::default()
        };
        eprintln!("\n=== {name} — post-synthesis state/input symbols ===");
        match sv_to_btor2(&primary, &yopts).and_then(|b| parse_btor2(&b)) {
            Ok(file) => {
                let mut names: Vec<String> = collect_symbols(&file).into_values().collect();
                names.sort();
                names.dedup();
                for n in names {
                    eprintln!("  {n}");
                }
            }
            Err(e) => eprintln!("  ERROR: {}", e.message),
        }
    }
}

// ============================================================================
// P3 — counterexample ↔ Verilator replay (claims-integrity Rule 9).
//
// The exact engine's `AG EF` recoverability counterexample (a reset→trap input
// sequence) is REPLAYED at RTL under Verilator: the design, driven by the
// witness's inputs, must actually reach and hold the trap state. A non-reproducing
// trace is a failure. This closes the loop — the model-level `Violated` verdict is
// backed by a concrete RTL execution, in the codebase (not a per-design agent run).
// ============================================================================

/// A self-contained SV FSM with a TERMINAL trap: `st` cycles 0→1→2→0, but `esc_i`
/// drives it to `st==3` which self-loops (terminal). `AG EF (st==0)` is Violated.
const TRAP_FSM_SV: &str = r#"module trapfsm (
  input  logic       clk_i,
  input  logic       rst_ni,
  input  logic       esc_i,
  output logic [1:0] st_o
);
  logic [1:0] st;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni)         st <= 2'd0;
    else if (esc_i)      st <= 2'd3;
    else if (st == 2'd3) st <= 2'd3;
    else if (st == 2'd2) st <= 2'd0;
    else                 st <= st + 2'd1;
  end
  assign st_o = st;
endmodule
"#;

#[test]
#[ignore = "requires slang + sv2v + yosys + verilator (mununu-sva image); run with --ignored"]
fn p3_verilator_replays_ag_ef_trap_counterexample() {
    use mununu_core::adapter::verilator::{
        TraceReplayConfig, VerilatorOptions, VerilatorTempDir, build_trace_replay_tb_cpp,
        compile_verilator, locate_verilator, parse_reset_simulation_dump,
    };

    // 1. verify_auto (exact-symbolic, reset-gated) on the trap FSM → Violated + a
    //    counterexample carrying the reset→trap INPUT sequence.
    let src =
        format!("// @mununu_guarantee nu Y. ((mu X. ((st == 0) or <> X)) and [] Y)\n{TRAP_FSM_SV}");
    let report = verify_auto(
        &[("trapfsm.sv".to_string(), src)],
        &YosysOptions {
            top: Some("trapfsm".to_string()),
            use_sv2v: true,
            ..Default::default()
        },
        &VerifyAutoOptions {
            exact_symbolic: true,
            config_values: [("rst_ni".to_string(), 1u64)].into_iter().collect(),
            ..Default::default()
        },
    )
    .expect("verify_auto");
    let prop = report.properties.first().expect("one property");
    assert!(
        matches!(prop.outcome, VerifyOutcome::Violated { .. }),
        "AG EF (st==0) must be Violated (esc→st==3 terminal trap); got {:?}",
        prop.outcome
    );
    let cx = prop
        .counterexample
        .as_ref()
        .expect("a Violated AG EF must carry a counterexample");
    assert!(
        !cx.inputs.is_empty(),
        "the counterexample must carry the replayable input sequence"
    );

    // 2. Build the replay trace: the witness inputs (drive esc_i to reach the trap), then a few
    //    esc_i=0 cycles to confirm the trap PERSISTS (terminal) at RTL.
    let mut trace: Vec<Vec<(String, u64)>> = cx.inputs.clone();
    for _ in 0..5 {
        trace.push(vec![("esc_i".to_string(), 0)]);
    }
    let cfg = TraceReplayConfig {
        top: "trapfsm".to_string(),
        clock_signal: "clk_i".to_string(),
        reset_signal: "rst_ni".to_string(),
        reset_asserted: 0, // active-low rst_ni
        hold_cycles: 2,
        held_inputs: vec![],
        trace,
        observe_registers: vec!["st_o".to_string()],
    };
    let tb = build_trace_replay_tb_cpp(&cfg).expect("render replay testbench");

    // 3. Compile the design + testbench under Verilator and run.
    let ver = locate_verilator().expect("verilator present (mununu-sva)");
    let tmp = VerilatorTempDir::new().expect("tempdir");
    let sv_path = tmp.path().join("trapfsm.sv");
    std::fs::write(&sv_path, TRAP_FSM_SV).expect("write sv");
    let opts = VerilatorOptions {
        top: Some("trapfsm".to_string()),
        ..Default::default()
    };
    let bin = compile_verilator(&ver.path, &opts, &sv_path, &tb, tmp.path())
        .expect("verilator compile+build");
    let out = std::process::Command::new(&bin).output().expect("run sim");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // 4. The final observed cycle must be the trap `st_o == 3` — the RTL reached AND held it.
    //    `parse_reset_simulation_dump` keeps the `cyc<N>:` prefix on each register name, so match
    //    on the suffix and value rather than the exact string.
    let dumps = parse_reset_simulation_dump(&stdout);
    let st_dumps: Vec<_> = dumps.iter().filter(|d| d.name.ends_with("st_o")).collect();
    assert!(
        !st_dumps.is_empty(),
        "expected at least one sampled st_o cycle; full dump: {stdout}"
    );
    let last = st_dumps.last().expect("at least one st_o sample");
    assert_eq!(
        last.value, 3,
        "RTL replay of the counterexample must end in the trap st_o==3; full dump: {stdout}"
    );
    // The trap is terminal: once entered it must PERSIST for every remaining esc_i=0 cycle. The
    // last 5 sampled cycles are the persistence tail appended in step 2 — all must read st_o==3.
    let tail = &st_dumps[st_dumps.len().saturating_sub(5)..];
    assert!(
        tail.iter().all(|d| d.value == 3),
        "trap st_o==3 must be absorbing across the persistence tail; full dump: {stdout}"
    );
}

/// REACH-RESCUE reducibility census — measures the real-corpus payoff of the
/// subprocess `reach_portfolio_rescue` BEFORE any surface wiring. For every corpus
/// design it runs the full internal portfolio (`PortfolioSequential`), collects
/// the properties it leaves ⊥, and reports how many of those are the reducible
/// `AG(state ⋈ value)` shape [`reduce_ag_invariant`] recognises. Only reducible ⊥
/// properties can be rescued by the subprocess portfolio, so this count is the
/// upper bound on the feature's corpus payoff. Diagnostic (no assertion) — the
/// `--nocapture` output is the deliverable. Needs only slang+sv2v+yosys (no
/// btormc/pono): reducibility is a pure formula-shape question.
#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn e2e_reach_rescue_reducibility_census() {
    use mununu_core::adapter::reach_rescue::reduce_ag_invariant;

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut total, mut definite, mut bot, mut reducible) = (0u32, 0u32, 0u32, 0u32);
    let mut designs_ran = 0u32;
    eprintln!(
        "\n============ reach-rescue reducibility census (portfolio-⊥ properties) ============"
    );
    for d in CORPUS {
        let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            corpus_report(d, Engine::PortfolioSequential)
        }));
        let report = match report {
            Ok(r) => r,
            Err(_) => {
                eprintln!("  {:28} SETUP-ERROR (skipped)", d.name);
                continue;
            }
        };
        designs_ran += 1;
        eprintln!("  {:28} {} properties", d.name, report.properties.len());
        for p in &report.properties {
            total += 1;
            if outcome_verdict(&p.outcome) != LedgerVerdict::Indefinite {
                definite += 1;
                continue;
            }
            bot += 1;
            let red = mununu_core::mu_calculus::parser::parse(&p.formula)
                .ok()
                .and_then(|f| reduce_ag_invariant(&f));
            let tag = match &red {
                Some(inv) => {
                    reducible += 1;
                    format!("REDUCIBLE({} {:?} {})", inv.signal, inv.op, inv.value)
                }
                None => "not-reducible".to_string(),
            };
            eprintln!("  {:28} {:36} ⊥  [{tag}]", d.name, p.name);
        }
    }
    std::panic::set_hook(prev);
    eprintln!("----------------------------------------------------------------------------------");
    eprintln!(
        "  designs ran: {designs_ran}/{}   total properties: {total}   definite: {definite}",
        CORPUS.len()
    );
    eprintln!("  portfolio-⊥ properties: {bot}   reducible to AG(state ⋈ value): {reducible}");
    eprintln!(
        "==================================================================================\n"
    );
}

// ── #468 sv mutate — the end-to-end flip oracle ────────────────────────────────
// A free-running 2-bit counter (`cnt`: 0→1→2→3→0 …). The recoverability property
// `AG EF (cnt == 3)` HOLDS on the design as-lifted (from every reachable state the
// counter cycles back through 3). A `stick:cnt` mutation freezes `cnt` at its reset
// value 0 → `cnt == 3` is unreachable → the property FLIPS to Violated. This is the
// property-adequacy signal `sv mutate` reports: the property genuinely constrains the
// counter's progress, so the injected fault is caught. Exercises the whole seam:
// VerifyAutoOptions.mutation → prepare_model → mutate::apply_mutation → verify → diff.
const COUNTER3_SV: &str = r#"module counter3 (
  input  logic       clk_i,
  input  logic       rst_ni,
  output logic [1:0] cnt_o
);
  logic [1:0] cnt;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) cnt <= 2'd0;
    else         cnt <= cnt + 2'd1;
  end
  assign cnt_o = cnt;
endmodule
"#;

#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn e2e_sv_mutate_stick_flips_recoverability() {
    use mununu_core::adapter::btor2::mutate::Mutation;
    use mununu_core::adapter::slang::verify_auto::mutate_and_compare;

    let src = format!(
        "// @mununu_guarantee nu Y. ((mu X. ((cnt == 3) or <> X)) and [] Y)\n{COUNTER3_SV}"
    );
    let sources = [("counter3.sv".to_string(), src)];
    let yopts = YosysOptions {
        top: Some("counter3".to_string()),
        use_sv2v: true,
        ..Default::default()
    };
    let base_opts = VerifyAutoOptions {
        exact_symbolic: true,
        config_values: [("rst_ni".to_string(), 1u64)].into_iter().collect(),
        ..Default::default()
    };

    let report = mutate_and_compare(&sources, &yopts, &base_opts, Mutation::Stick("cnt".into()))
        .expect("mutate_and_compare runs the baseline + mutant lifts");

    assert_eq!(report.mutation, "stick:cnt");
    let flip = report
        .properties
        .iter()
        .find(|p| p.baseline == "holds")
        .expect("the recoverability property holds at baseline");
    assert!(
        flip.flipped && flip.mutant == "violated",
        "freezing cnt must FLIP AG EF(cnt==3) holds→violated (cnt stuck at 0 never reaches 3); \
         got baseline={} mutant={}",
        flip.baseline,
        flip.mutant
    );
    assert!(
        report.flipped >= 1,
        "the mutation must be caught by ≥1 property"
    );
}

// #468b off-by-one — a counter that wraps at 8 (`cnt == 8 → 0`). `AG (cnt != 9)`
// HOLDS on the design as-lifted (cnt maxes at 8). `off-by-one:cnt` perturbs the
// wrap bound `cnt == 8` to `cnt == 9`, so the counter now reaches 9 before wrapping
// → the property FLIPS to Violated. Validates the targeting transform through the
// real lift + verify.
const WRAP_CTR_SV: &str = r#"module wrapctr (
  input  logic       clk_i,
  input  logic       rst_ni,
  output logic [3:0] cnt_o
);
  logic [3:0] cnt;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni)          cnt <= 4'd0;
    else if (cnt == 4'd8) cnt <= 4'd0;
    else                  cnt <= cnt + 4'd1;
  end
  assign cnt_o = cnt;
endmodule
"#;

#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn e2e_sv_mutate_off_by_one_flips_a_threshold_property() {
    use mununu_core::adapter::btor2::mutate::Mutation;
    use mununu_core::adapter::slang::verify_auto::mutate_and_compare;

    let src = format!("// @mununu_guarantee nu X. ((cnt != 9) and [] X)\n{WRAP_CTR_SV}");
    let sources = [("wrapctr.sv".to_string(), src)];
    let yopts = YosysOptions {
        top: Some("wrapctr".to_string()),
        use_sv2v: true,
        ..Default::default()
    };
    let base_opts = VerifyAutoOptions {
        exact_symbolic: true,
        config_values: [("rst_ni".to_string(), 1u64)].into_iter().collect(),
        ..Default::default()
    };

    let report = mutate_and_compare(
        &sources,
        &yopts,
        &base_opts,
        Mutation::parse("off-by-one:cnt").expect("parse off-by-one:cnt"),
    )
    .expect("mutate_and_compare runs baseline + mutant");

    let flip = report
        .properties
        .iter()
        .find(|p| p.baseline == "holds")
        .expect("AG(cnt!=9) holds at baseline (cnt wraps at 8)");
    assert!(
        flip.flipped && flip.mutant == "violated",
        "moving the wrap bound 8→9 must FLIP AG(cnt!=9) holds→violated; got baseline={} mutant={}",
        flip.baseline,
        flip.mutant
    );
}
