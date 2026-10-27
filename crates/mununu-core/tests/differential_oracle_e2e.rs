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
use std::path::{Path, PathBuf};

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
/// # in the mununu-sva image (btormc + pono + rIC3 on PATH), point at a leaf dir of .btor2,
/// # and give SPACER its isolated child (a release binary): run in-process, z3's Fixedpoint
/// # SIGSEGVs on `vis_arrays_bufferAlloc` and takes the whole test with it (measured 2026-10-19)
/// MUNUNU_SELF_EXE=/cargo-target/release/mununu MUNUNU_HWMCC_DIR=/tmp/hwmcc20/<btor-subdir> \
///   cargo test -p mununu-core --test differential_oracle_e2e \
///   hwmcc_adjudication_over_user_dir -- --ignored --nocapture
/// ```
///
/// mununu#738 — one instance must not void the table: `MUNUNU_HWMCC_SKIP=<substr>[,<substr>…]`
/// skips every file whose name contains a listed substring (printed as `SKIPPED`), and every
/// instance runs under a run budget of `MUNUNU_HWMCC_TIMEOUT_MS` (default 300 000) — the
/// members poll it, so an instance no member decides abstains at the deadline. Measured
/// 2026-10-20 on `vis_arrays_bufferAlloc` member by member in `mununu-sva`: the in-process z3
/// Fixedpoint SIGSEGVs in 4 s (the crash the isolated child contains); no other member crashes;
/// `--owned-only --timeout-ms 120000` abstains at 120 s; rIC3, btormc and pono give no
/// answer in 120 s; the UNBUDGETED full portfolio never returns (this harness sat on it for
/// over two hours). The run budget bounds every member that polls it — NOT the exact engine's
/// bit-blast phase, which the full portfolio runs on the calling thread with no clock (a
/// 120-s budget sat on `vis_arrays_bufferAlloc` for two hours again on 2026-10-21, and so did
/// `MUNUNU_BDD_TIME_BUDGET_MS=60000`), so the skip list is the containment for that one. Both
/// are harness knobs — the shipped verdicts never depend on them.
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
    let budget_ms: u64 = std::env::var("MUNUNU_HWMCC_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300_000);
    let skip: Vec<String> = std::env::var("MUNUNU_HWMCC_SKIP")
        .ok()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for path in &files {
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if skip.iter().any(|s| fname.contains(s.as_str())) {
            eprintln!("  {fname:44} SKIPPED (MUNUNU_HWMCC_SKIP)");
            continue;
        }
        total += 1;
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
        // mununu#738 — a per-instance run budget the members poll (the exact engine between its
        // stages, the z3 members per query, the subprocesses as a wall limit).
        let budget = mununu_core::adapter::run_budget::Budget::with_ms(budget_ms);
        let out = {
            let _g = mununu_core::adapter::run_budget::enter(&budget);
            decide_reach_portfolio(&file)
        };
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
    /// production default (`symbolic_engine=false, exact_symbolic=false`), with the mununu#663
    /// `auto` must-edge policy (per-target ∀∃ must on a μ-under-ν formula).
    Cegar,
    /// The same explicit engine with the pre-#663 posture — may-only edges on every property
    /// (`must_edge_auto = false`, `must_edge_inference = Off`). On a μ under a ν its `◇` can
    /// never turn true, so an `AG EF` / `AG AF` is structurally ⊥: the gap `auto` closes and the
    /// portfolio used to close for it. The ⊥ ESCALATION is off too (`rescue_bottom_liveness =
    /// false`): this variant measures the engine's own posture, and since 2026-10-15 the
    /// liveness-to-safety rescue reduces the antecedent-free `AG AF b` and would close the gap
    /// behind the engine's back (see `CegarMayOnlyRescued`).
    CegarMayOnly,
    /// `CegarMayOnly` WITH the ⊥ escalation on (the default): what a user running the may-only
    /// engine actually gets. The difference to `CegarMayOnly` is the liveness rescue's reach.
    CegarMayOnlyRescued,
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
            Engine::Cegar | Engine::CegarMayOnly | Engine::CegarMayOnlyRescued => {
                (false, false, None)
            }
            Engine::SymbolicCube => (true, false, None),
            Engine::ExactSymbolic => (false, true, None),
            Engine::PortfolioSequential => (false, false, Some(PortfolioMode::Sequential)),
            Engine::PortfolioParallel => (false, false, Some(PortfolioMode::Parallel)),
        };
        opts.symbolic_engine = sym;
        opts.exact_symbolic = exact;
        opts.portfolio = portfolio;
        if matches!(self, Engine::CegarMayOnly | Engine::CegarMayOnlyRescued) {
            opts.must_edge_auto = false;
        }
        if matches!(self, Engine::CegarMayOnly) {
            opts.rescue_bottom_liveness = false;
        }
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
        // the block's own `prim_assert.sv` stub defines INC_ASSERT; the files that cannot be
        // lifted under it carry a `_lift.sv` variant (see `lift_variants`)
        lift_overrides: lift_variants(&root.join(d.dir).join("source")),
        ..Default::default()
    };
    engine.apply(&mut opts);
    verify_auto(&sources, &yopts, &opts)
        .unwrap_or_else(|e| panic!("{}: verify_auto failed: {}", d.name, e.message))
}

/// `<stem>_lift.sv` beside a source replaces `<stem>.sv` for the LIFT only
/// (`VerifyAutoOptions::lift_overrides`); extraction keeps reading the vendored file. Written by
/// `scripts/vendor_opentitan_ip.py --strip-inc-assert` for the two upstream files whose
/// `ifdef INC_ASSERT` region no frontend lifts — the alert sender's `sequence` blocks, the
/// sparse-FSM flop's enum-method loop — now that the synthesis shims define `INC_ASSERT`
/// (2026-10-22) so the assertion-support logic (`fpv_err_present`) is in the model.
fn lift_variants(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix("_lift.sv") {
            let text = std::fs::read_to_string(e.path())
                .unwrap_or_else(|err| panic!("{}: {err}", e.path().display()));
            out.push((format!("{stem}.sv"), text));
        }
    }
    out.sort();
    out
}

/// The lift of a vendored OpenTitan IP (`examples/verify/ip_opentitan_*/source`): the generated
/// synthesis shim in place of the real `prim_assert.sv`, the real `.svh` macro files blanked,
/// plus every `_lift.sv` variant — exactly what the IP's `run.sh` passes as `--lift-override`.
fn ip_lift_overrides(dir: &Path) -> Vec<(String, String)> {
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(f).display()))
    };
    let mut v = vec![
        ("prim_assert.sv".to_string(), read("prim_assert_shim.sv")),
        ("prim_assert_standard_macros.svh".to_string(), String::new()),
        ("prim_assert_sec_cm.svh".to_string(), String::new()),
    ];
    v.extend(lift_variants(dir));
    v
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
            "prim_buf.sv",
            "prim_diff_decode.sv",
            "prim_flop.sv",
            "prim_sec_anchor_buf.sv",
            "prim_sec_anchor_flop.sv",
            "prim_xnor2.sv",
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
            "prim_count.sv",
            "prim_count_pkg.sv",
            "prim_flop.sv",
            "prim_mubi4_sender.sv",
            "prim_sparse_fsm_flop.sv",
            "rom_ctrl_compare.sv",
            "rom_ctrl_counter.sv",
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
        // mununu#656 — `prim_leading_one_ppc` (the priority prefix) is vendored too; without it the
        // lift black-boxes it and `gnt_o` reads its outputs as free inputs, so the property is
        // (rightly) refused as a verdict over a chaotic stub.
        files: &[
            "prim_arbiter_ppc.sv",
            "prim_assert.sv",
            "prim_util_pkg.sv",
            "prim_leading_one_ppc.sv",
        ],
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
        files: &[
            "prim_esc_sender.sv",
            "prim_assert.sv",
            "prim_esc_pkg.sv",
            "prim_buf.sv",
            "prim_diff_decode.sv",
            "prim_sec_anchor_buf.sv",
            "prim_xnor2.sv",
        ],
        top: "prim_esc_sender",
        annotations: &["nu Y. ((mu X. ((state_q == 0) or <> X)) and [] Y)"], // Idle = 0
        config: &[("rst_ni", 1)],
        ledger: &[("(state_q == 0)", LedgerVerdict::True)], // HOLDS (esc_sender recovers to Idle)
    },
    // prim_alert_sender — 7-state alert-sender FSM (Idle=0), the alert-path sibling of
    // prim_esc_sender. Since mununu#750 the instantiated prim_diff_decode / prim_sec_anchor_* /
    // prim_flop_2sync submodules are vendored (upstream 558921c); before, they were black-boxed and
    // `AlertTest1_A` shipped a VIOLATED that HOLDS on the full closure.
    // AG EF Idle recoverability HOLDS, as for esc_sender (see the mununu#651 note on the
    // ledger row: the earlier VIOLATED reading was a two-player argument). Reset is gated
    // (rst_ni=1) and Idle=0 is the real idle state, not a vacuous atom.
    CorpusDesign {
        name: "prim_alert_sender",
        dir: "dc_opentitan_prim_alert_sender",
        files: &[
            "prim_alert_sender.sv",
            "prim_assert.sv",
            "prim_alert_pkg.sv",
            "prim_buf.sv",
            "prim_diff_decode.sv",
            "prim_flop.sv",
            "prim_flop_2sync.sv",
            "prim_sec_anchor_buf.sv",
            "prim_sec_anchor_flop.sv",
            "prim_util_pkg.sv",
        ],
        top: "prim_alert_sender",
        annotations: &["nu Y. ((mu X. ((state_q == 0) or <> X)) and [] Y)"], // Idle = 0
        config: &[("rst_ni", 1)],
        // mununu#651 (2026-10-10) — CORRECTED False → True. The July row read VIOLATED; on the
        // current lift two independent engines decide HOLDS on the same BTOR2 — the exact
        // engine (full-state ROBDD, once the #651 refusal stopped skipping it) and the
        // `btor2 verify-recoverability` cube ladder (explicit predicate-cube), which never went
        // through that refusal — and the RTL agrees: every phase returns to Idle via
        // Pause0 → Pause1, `sigint_detected` forces Idle, `default` is Idle (its own
        // `InBandInitFsm_A`). `AG EF` quantifies the free stub outputs EXISTENTIALLY, so "an
        // adversarial never-acking environment" (the old note's argument) is a two-player
        // reading, not CTL's. The July VIOLATED could not be reproduced; its cause is unknown.
        ledger: &[("(state_q == 0)", LedgerVerdict::True)],
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
            "prim_buf.sv",
            "prim_flop.sv",
            "prim_mubi4_sender.sv",
            "prim_sec_anchor_buf.sv",
            "prim_sparse_fsm_flop.sv",
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
            // mununu#656 — `prim_fifo_sync_cnt` and its own `prim_count` are vendored too; without
            // them the lift black-boxes the pointer logic and `depth_o` reads a chaotic stub.
            "prim_fifo_sync_cnt.sv",
            "prim_count.sv",
            "prim_count_pkg.sv",
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
        // mununu#750 (2026-10-21) — RECORDED ⊥, was False. The VIOLATED came from the exact engine
        // on a model whose seven missing modules (keymgr_err, keymgr_op_state_ctrl, prim_count, …)
        // left undriven nets in the FSM's cone: a may-trace, now withheld as
        // `violation-over-free-leaves`. On the vendored closure (scripts/vendor_opentitan_ip.py, no
        // black boxes) the row did not decide within 30 min, so its truth is not established here.
        ledger: &[("(state_q == 865)", LedgerVerdict::Indefinite)],
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
///      form was still ⊥ (mununu#562). On 2026-10-11 (mununu#663) the default engine decided
///      BOTH — its `auto` policy runs per-target ∀∃ must edges on a μ-under-ν formula — so the
///      gap this gate needs is now stated on the engine's PRE-#663 posture (`CegarMayOnly`:
///      may-only edges, whose `◇` can never turn true), which still leaves the box form ⊥ and
///      PRINTS it. If even the may-only cube decides both, this fails loudly: the gap is gone
///      and the test must move to a design that still has one, not be weakened.
///   2. mununu#663 — the default engine (`auto`) decides both uart_tx properties by itself,
///      matching the exact oracle (the gain that retired step 1's original premise; measured
///      2026-10-11: box form VIOLATED, diamond form HOLDS at the cube's main pass).
///   3. Both portfolio modes DECIDE both properties, matching the exact oracle (AG-AF=Violated,
///      AG-EF=Holds) — the portfolio recovers what the may-only engine misses.
///   4. Sequential and Parallel agree exactly (same engines, same merge).
///   5. No `portfolio-soundness-alarm` note fired (the runtime guard stayed quiet) and a
///      `portfolio` provenance note is present.
#[test]
#[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
fn e2e_portfolio_decides_what_the_default_engine_misses() {
    let uart = &CORPUS[0];
    assert_eq!(
        uart.name, "uart_tx",
        "expected uart_tx as the first corpus design"
    );

    // 1. Baseline: the may-only explicit engine (the pre-#663 posture) leaves at least one
    //    uart_tx liveness prop ⊥.
    let cegar = run_corpus_verdicts(uart, Engine::CegarMayOnly);
    let still_bottom: Vec<&str> = cegar
        .iter()
        .filter(|(_, v, _)| *v == LedgerVerdict::Indefinite)
        .map(|(n, _, _)| *n)
        .collect();
    eprintln!(
        "may-only Cegar alone leaves ⊥ on uart_tx: {still_bottom:?} (of {} properties)",
        cegar.len()
    );
    assert!(
        !still_bottom.is_empty(),
        "precondition: the may-only Cegar must leave at least one uart_tx property ⊥ (the gap \
         the portfolio — and since mununu#663 the `auto` policy — closes) — if it decides both, \
         this gate has no gap left to test on this design and must move to one that has; got \
         {cegar:?}"
    );

    // 1b. 2026-10-15 — the liveness rescue's reach on this design. With the ⊥ escalation on, the
    //     may-only engine's run ends with the box form DECIDED, matching the exact oracle: the
    //     antecedent-free `AG AF (bit_cnt_q == 0)` now reduces to the liveness-to-safety monitor
    //     (`liveness_rescue::reduce_response_shape`). Measured, not assumed — if this regresses
    //     to ⊥, the rescue lost a shape.
    let rescued = run_corpus_verdicts(uart, Engine::CegarMayOnlyRescued);
    let rescued_box = rescued
        .iter()
        .find(|(n, _, _)| *n == "(mu Y. ((bit_cnt_q == 0) or [] Y))")
        .unwrap_or_else(|| panic!("box form missing from {rescued:?}"));
    eprintln!("may-only Cegar + ⊥ escalation on uart_tx: {rescued:?}");
    assert_eq!(
        rescued_box.1,
        LedgerVerdict::False,
        "the liveness rescue decides AG AF (bit_cnt_q == 0) as the exact oracle does: {rescued:?}"
    );

    // 2. mununu#663 — the default engine (`auto`: per-target must on a μ under a ν) decides
    //    both by itself and agrees with the exact oracle.
    let auto = run_corpus_verdicts(uart, Engine::Cegar);
    let auto_v: Vec<_> = auto.iter().map(|(n, v, _)| (*n, *v)).collect();
    for (needle, want) in [
        ("(mu Y. ((bit_cnt_q == 0) or [] Y))", LedgerVerdict::False),
        ("(mu X. ((bit_cnt_q == 0) or <> X))", LedgerVerdict::True),
    ] {
        let got = auto_v
            .iter()
            .find(|(n, _)| *n == needle)
            .unwrap_or_else(|| panic!("auto Cegar result missing `{needle}`: {auto_v:?}"));
        assert_eq!(
            got.1, want,
            "mununu#663: the default engine's `auto` must-edge policy decides `{needle}` on its \
             own and agrees with the exact oracle; got {auto_v:?}"
        );
    }

    // 3 + 4. Both portfolio modes decide both properties, and agree with each other.
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
    // The Verilator replay drives `u64` values; every cell of this witness fits (a wider one
    // would be refused here rather than truncated — mununu#723).
    let mut trace: Vec<Vec<(String, u64)>> = cx
        .inputs
        .iter()
        .map(|f| {
            f.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_u64().expect("a replayable value fits 64 bits"),
                    )
                })
                .collect()
        })
        .collect();
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

/// OpenTitan's OWN SVA through `sv verify-auto` — a census, not a gate. For every corpus design it
/// runs the vendored assertions as they are (no `@mununu_guarantee` annotations, the design's
/// reset/config pins, the CLI's default `portfolio-sequential` engine) and prints one
/// tab-separated `CENSUS` line per assertion: outcome class, detail (a ⊥ / skip reason or the
/// unsupported reason) and the per-property translator notes (`two-state-fold`,
/// `strong-sequence-reading`). Diffing two commits' output measures what an SVA-translator change
/// moved on real upstream assertions — including how much of a gain is merely a `$isunknown` fold,
/// which holds trivially and must not be counted as a decided check. SVA extraction reads the real
/// OpenTitan macros. The lift reads them too — assertion-side logic (`fpv_err_present`, `err_d`)
/// exists only then — unless that lift FAILS (`ASSERT_FPV_LINEAR_FSM`, the senders' sequence
/// blocks): then it is retried on the design's own synthesis shim (which defines `INC_ASSERT`
/// too, with a `_lift.sv` variant for each file that cannot be lifted under it) through
/// `VerifyAutoOptions::lift_overrides`, and the design's `census:` line says `lift=shim`.
#[test]
#[ignore = "census: requires slang + sv2v + yosys (mununu-sva image); run with --ignored --nocapture"]
fn e2e_opentitan_sva_census() {
    use mununu_core::adapter::slang::verify_auto::PortfolioMode;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
    for d in CORPUS {
        let read = |file: &str| {
            let p = root.join(d.dir).join("source").join(file);
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        // The corpus's `prim_assert.sv` is the synthesis shim that expands every `ASSERT` macro
        // to NOTHING; swap in the genuine OpenTitan macro set (vendored with the M.0 fixture), as
        // `e2e_csrng_real_sva_verdict_breakdown` does, or the census sees no assertion at all.
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let mut sources: Vec<(String, String)> = d
            .files
            .iter()
            .filter(|f| **f != "prim_assert.sv")
            .map(|f| (f.to_string(), read(f)))
            .collect();
        for f in [
            "prim_assert.sv",
            "prim_assert_standard_macros.svh",
            "prim_assert_sec_cm.svh",
            "prim_flop_macros.sv",
        ] {
            if !sources.iter().any(|(n, _)| n == f) {
                let text = std::fs::read_to_string(prim.join(f)).expect("M.0 macro file");
                sources.push((f.to_string(), text));
            }
        }
        let yopts = YosysOptions {
            top: Some(d.top.to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        // The lift sees exactly the corpus's own sources: its `prim_assert.sv` shim in place of
        // the real one, and the real macro files the corpus never carried blanked — staged, they
        // would re-define the `ASSERT` macros the shim compiles out.
        let mut lift_overrides: Vec<(String, String)> = sources
            .iter()
            .filter_map(|(name, _)| {
                if d.files.contains(&name.as_str()) {
                    (name == "prim_assert.sv").then(|| (name.clone(), read(name)))
                } else {
                    Some((name.clone(), String::new()))
                }
            })
            .collect();
        lift_overrides.extend(lift_variants(&root.join(d.dir).join("source")));
        let opts = VerifyAutoOptions {
            config_values: d.config.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            portfolio: Some(PortfolioMode::Sequential),
            lift_overrides,
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let real_lift = VerifyAutoOptions {
            lift_overrides: Vec::new(),
            ..opts.clone()
        };
        let mut lift = "real";
        let run = match verify_auto(&sources, &yopts, &real_lift) {
            Err(e) if e.message.contains("SV → BTOR2") => {
                lift = "shim";
                verify_auto(&sources, &yopts, &opts)
            }
            other => other,
        };
        let report = match run {
            Ok(r) => r,
            Err(e) => {
                println!(
                    "CENSUS\t{}\t-\t-\terror\t{}",
                    d.name,
                    e.message.replace(['\n', '\t'], " ")
                );
                continue;
            }
        };
        for p in &report.properties {
            let (class, detail) = match &p.outcome {
                VerifyOutcome::Holds => ("holds", String::new()),
                VerifyOutcome::Violated { .. } => ("violated", String::new()),
                VerifyOutcome::Unknown { .. } => (
                    "unknown",
                    p.bottom_reason
                        .as_ref()
                        .map(|b| format!("{b:?}"))
                        .unwrap_or_default(),
                ),
                VerifyOutcome::Skipped { reason } => ("skipped", reason.clone()),
            };
            let notes: Vec<&str> = report
                .notes
                .iter()
                .filter(|n| n.property.as_deref() == Some(p.name.as_str()))
                .filter(|n| n.kind == "two-state-fold" || n.kind == "strong-sequence-reading")
                .map(|n| n.kind.as_str())
                .collect();
            println!(
                "CENSUS\t{}\t{}\t{}\t{class}\t{}\t{}",
                d.name,
                p.name,
                p.label.as_deref().unwrap_or("-"),
                detail
                    .replace(['\n', '\t'], " ")
                    .chars()
                    .take(160)
                    .collect::<String>(),
                notes.join(",")
            );
        }
        for (name, reason) in &report.unsupported {
            println!(
                "CENSUS\t{}\t{name}\t-\tunsupported\t{}\t",
                d.name,
                reason
                    .replace(['\n', '\t'], " ")
                    .chars()
                    .take(160)
                    .collect::<String>()
            );
        }
        eprintln!(
            "census: {} in {:.1}s (lift={lift})",
            d.name,
            t0.elapsed().as_secs_f64()
        );
    }
}

/// The first FULL OpenTitan IP in the corpus — `edn` (TL-UL register block + core + 8 endpoint FSMs,
/// 200 registers; `examples/verify/ip_opentitan_edn`, lowRISC/opentitan 558921c): its own 52
/// assertions plus the system-level `@mununu_guarantee` properties of `mununu_properties.sv`, through
/// `sv verify-auto` exactly as the example's `run.sh` does — the SVA extracted from the real
/// `prim_assert` macros, the lift under the generated synthesis shim, `rst_ni` pinned inactive, the
/// bus free. The pins are the measured verdicts (ledger §3.3b-6, 2026-10-18); a definite verdict
/// that moves is a finding, not a re-baseline, and a pinned ⊥ that decides is the engine
/// improvement announcing itself.
/// mununu#806 — a lifted register name must not embed the per-run yosys staging directory.
/// yosys names a `$func$` temporary after the source path it was given; with the staged
/// `preprocessed.sv` handed over by its absolute path, three edn registers
/// (`…u_state_regs.sv2v_cast_F2F2E$func$/tmp/mununu-yosys-<pid>-…/preprocessed.sv:3983$4411.$result`)
/// differed between any two runs of the same design. The lift now runs yosys inside the staging
/// directory and names the file bare, so the names are `…$func$preprocessed.sv:3983$4411.$result`
/// on every run. Two lifts, byte-identical state names, none with the temp path.
#[test]
#[ignore = "requires sv2v + Yosys (use the mununu-sva docker image); run with --ignored --nocapture"]
fn e2e_lifted_register_names_carry_no_per_run_temp_path() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/verify/ip_opentitan_edn/source");
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(f).display()))
    };
    let order: Vec<String> = read("sources.txt")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let additional: Vec<(String, String)> = order
        .iter()
        .filter(|f| *f != "edn.sv")
        .map(|f| (f.clone(), read(f)))
        .collect();
    let yopts = YosysOptions {
        top: Some("edn".to_string()),
        use_sv2v: true,
        additional_sources: additional,
        ..Default::default()
    };
    let (primary, yopts) = mununu_core::adapter::slang::verify_auto::apply_lift_overrides(
        "edn.sv",
        &read("edn.sv"),
        &yopts,
        &ip_lift_overrides(&dir),
    )
    .expect("the edn lift overrides apply");
    // The names a report shows: `verify_auto` flattens the symbols (`flat_symbol_names`) and the
    // parser's `collect_symbols` reads them off the state lines.
    let state_names = |btor2: &str| -> Vec<String> {
        let flat = mununu_core::adapter::btor2::parser::flat_symbol_names(btor2);
        let file = mununu_core::adapter::btor2::parser::parse(&flat).expect("the lift parses");
        let symbols = mununu_core::adapter::btor2::parser::collect_symbols(&file);
        // `<nid> state <sort> …` lines, by nid; their names from the parser's symbol table.
        let mut names: Vec<(i64, String)> = flat
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let nid: i64 = w.next()?.parse().ok()?;
                (w.next()? == "state").then_some(nid)
            })
            .filter_map(|nid| symbols.get(&nid).map(|s| (nid, s.clone())))
            .collect();
        names.sort();
        names.into_iter().map(|(_, s)| s).collect()
    };
    // `verify_auto`'s two-step lift: the bare lift black-boxes the prim flops (3 registers survive),
    // so re-lift with the stubs for the cut modules exactly as `verify_auto` does (`auto_stub_flops`).
    let lift = |label: &str| -> String {
        let bare = mununu_core::adapter::yosys::sv_to_btor2_with_blackboxes(&primary, &yopts)
            .expect("the edn IP lifts");
        let stubs = mununu_core::adapter::slang::prim_stubs::stubs_for_cut_modules(
            &bare.blackboxed_modules,
        );
        let mut with_stubs = yopts.clone();
        with_stubs.additional_sources.extend(stubs.iter().cloned());
        let lifted =
            mununu_core::adapter::yosys::sv_to_btor2_with_blackboxes(&primary, &with_stubs)
                .expect("the edn IP lifts with the flop stubs");
        eprintln!(
            "{label}: {} modules black-boxed before the stubs, {} stubs, {} BTOR2 lines",
            bare.blackboxed_modules.len(),
            stubs.len(),
            lifted.btor2.lines().count()
        );
        lifted.btor2
    };
    let t0 = std::time::Instant::now();
    let first = lift("first lift");
    let second = lift("second lift");
    if let Some(dump) = std::env::var_os("MUNUNU_E2E_LIFT_DUMP") {
        std::fs::write(&dump, &first).expect("the lift dump is writable");
    }
    let (a, b) = (state_names(&first), state_names(&second));
    eprintln!(
        "edn lifted twice: {} / {} state registers, {:.0}s",
        a.len(),
        b.len(),
        t0.elapsed().as_secs_f64()
    );
    assert!(
        a.len() > 100,
        "the edn lift has {} state registers",
        a.len()
    );
    let leaked: Vec<&String> = a
        .iter()
        // The staging prefix, not `/tmp/`: macOS stages under `$TMPDIR` (`/var/folders/…/T/`).
        .filter(|n| n.contains("mununu-yosys-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "state names embed the per-run yosys staging directory: {leaked:?}"
    );
    assert_eq!(
        a, b,
        "two lifts of the same design disagree on their state names"
    );
}

#[test]
#[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored --nocapture"]
fn e2e_opentitan_ip_edn_full() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/verify/ip_opentitan_edn/source");
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(f).display()))
    };
    // `sources.txt` is the compile order with the primary last; verify_auto wants the primary first.
    let order: Vec<String> = read("sources.txt")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut sources: Vec<(String, String)> = vec![("edn.sv".to_string(), read("edn.sv"))];
    sources.extend(
        order
            .iter()
            .filter(|f| *f != "edn.sv")
            .map(|f| (f.clone(), read(f))),
    );
    sources.push((
        "mununu_properties.sv".to_string(),
        read("mununu_properties.sv"),
    ));
    let yopts = YosysOptions {
        top: Some("edn".to_string()),
        use_sv2v: true,
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    let opts = VerifyAutoOptions {
        config_values: [("rst_ni".to_string(), 1u64)].into_iter().collect(),
        portfolio: Some(PortfolioMode::Sequential),
        lift_overrides: ip_lift_overrides(&dir),
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let report = verify_auto(&sources, &yopts, &opts).expect("the full IP lifts and runs");
    eprintln!(
        "edn full IP: {} properties, {} unsupported, {} registers, {:.0}s",
        report.properties.len(),
        report.unsupported.len(),
        report.diagnostics.state_register_count,
        t0.elapsed().as_secs_f64()
    );
    for p in &report.properties {
        // a ⊥ row names its cause (budget-expired / engine-did-not-complete with the members'
        // refusals / …) so a failing pin is diagnosable from the transcript alone
        let reason = p
            .bottom_reason
            .as_ref()
            .map(|r| format!(" [{}]", r.one_line()))
            .unwrap_or_default();
        eprintln!(
            "  {:<40} {:<10} {}{reason}",
            p.label.as_deref().unwrap_or(&p.name),
            p.outcome.label(),
            p.decided_by.as_deref().unwrap_or("-")
        );
        // ... and its notes (which members ran, refused or declined), for the same reason
        if p.outcome.label() == "unknown" {
            for n in report
                .notes
                .iter()
                .filter(|n| n.property.as_deref() == Some(p.name.as_str()))
            {
                eprintln!("      note {}: {} {:?}", n.kind, n.summary, n.items);
            }
        }
    }
    // The whole closure is modelled: nothing black-boxed (the properties file is an empty PACKAGE;
    // an empty module would also pass since mununu#724), the register block and the core both present.
    assert!(
        report.diagnostics.blackboxed_modules.is_empty(),
        "{:?}",
        report.diagnostics.blackboxed_modules
    );
    assert!(
        report.diagnostics.state_register_count >= 180,
        "{}",
        report.diagnostics.state_register_count
    );
    let rows = |l: &str| -> Vec<&VerifyOutcome> {
        report
            .properties
            .iter()
            .filter(|p| p.label.as_deref() == Some(l))
            .map(|p| &p.outcome)
            .collect()
    };
    let by = |l: &str| -> Vec<Option<&str>> {
        report
            .properties
            .iter()
            .filter(|p| p.label.as_deref() == Some(l))
            .map(|p| p.decided_by.as_deref())
            .collect()
    };
    let holds = |l: &str| rows(l).iter().all(|o| matches!(o, VerifyOutcome::Holds));
    // --- the IP's own SVA ---
    // Error → alert: a fault flag deep in a sub-instance (`u_edn_core.u_edn_main_sm.u_state_regs`,
    // `gen_ep_blk[k].u_edn_ack_sm_ep.u_state_regs`, `u_reg.u_prim_reg_we_check`) reaches
    // `alert_tx_o[1].alert_p` within 30 cycles — the system-level rows mununu#722 made checkable.
    assert_eq!(rows("FpvSecCmMainFsmCheck_A").len(), 1);
    assert!(holds("FpvSecCmMainFsmCheck_A"));
    assert_eq!(
        rows("FpvSecCmAckFsmCheck_A").len(),
        8,
        "one row per endpoint instance"
    );
    assert!(holds("FpvSecCmAckFsmCheck_A"));
    assert!(holds("FpvSecCmRegWeOnehotCheck_A"));
    // `ASSERT_KNOWN` folds to a constant (the model is 2-state) and is decided without a cone.
    assert!(holds("TlDValidKnownO_A") && holds("AlertTxKnownO_A"));
    assert_eq!(by("AlertTxKnownO_A"), vec![Some("constant")]);
    assert_eq!(rows("EdnEndPointOut_A").len(), 8);
    assert!(holds("EdnEndPointOut_A"));
    // `alert[1] |-> edn_o[i].edn_ack == 0`: a software-triggerable fatal error through the free bus
    // in the cycle an endpoint acks — reachable from reset, a spec gap in the assertion. All EIGHT
    // symmetric rows decide VIOLATED with a trace (mununu#726: under the rescue's fixed 5-s
    // per-query timeout one to three of them decided per run, load-dependent).
    let fatal = rows("EdnFatalAlertNoRsp_A");
    assert_eq!(fatal.len(), 8);
    // mununu#723 — the 109-bit `tl_i` (the bus write that raises the fatal alert) is listed in
    // the trace at its full width; it used to be the one input missing, silently.
    assert!(
        report
            .properties
            .iter()
            .filter(|p| p.label.as_deref() == Some("EdnFatalAlertNoRsp_A"))
            .any(|p| {
                p.counterexample.as_ref().is_some_and(|cx| {
                    cx.inputs
                        .iter()
                        .flatten()
                        .any(|(k, v)| k == "tl_i" && v.bits() == 109)
                })
            }),
        "a violated row's trace must list `tl_i` at 109 bits"
    );
    assert!(
        fatal
            .iter()
            .all(|o| matches!(o, VerifyOutcome::Violated { .. })),
        "{fatal:?}"
    );
    assert!(
        report
            .properties
            .iter()
            .filter(|p| p.label.as_deref() == Some("EdnFatalAlertNoRsp_A"))
            .all(|p| p.counterexample.is_some()),
        "every violated row carries its trace"
    );
    // `EdnDataStable_A` ×8 (`$stable(bus) throughout (req || !enable)[->1]`) translates since the
    // weak-until lowering of `throughout b[->1]` (2026-10-18): nothing is unsupported any more. Both
    // bus-stability families HOLD through the reach rescue's bounded-chain monitor (PR C, 2026-10-18;
    // the weak-until block judged by a pending latch) — native k-induction, 1–5 s a row. SymbiYosys
    // `abc pdr` proves the same assertions for all eight endpoints in 18 s, and a deliberately false
    // control over the same pending latch fails at step 8, so the proofs are not vacuous.
    assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
    for l in ["EdnDataStable_A", "EdnDataStableDisable_A"] {
        assert_eq!(rows(l).len(), 8, "{l}");
        assert!(holds(l), "{l}: {:?}", rows(l));
        assert!(
            by(l).iter().all(|d| *d == Some("reach-rescue")),
            "{l}: {:?}",
            by(l)
        );
    }
    // The five duplicated-counter fault-flag chains: their proof is the invariant "the two counter
    // copies agree", which k-induction cannot find from a free step state and z3 SPACER does not find
    // within the rescue slice (nor within 300 s). Since the rIC3 member (bit-level IC3, 2026-10-19,
    // mununu#736) they HOLD through the reach rescue in 2–5 s a row; SymbiYosys `abc pdr` proves
    // `!err_o` in 8 s.
    for l in [
        "FpvSecCmCntAlertCheck_A",
        "FpvSecCmResCmdFifoWptrCheck_A",
        "FpvSecCmResCmdFifoRptrCheck_A",
        "FpvSecCmGenCmdFifoWptrCheck_A",
        "FpvSecCmGenCmdFifoRptrCheck_A",
    ] {
        assert_eq!(rows(l).len(), 1, "{l}");
        assert!(holds(l), "{l}: {:?}", rows(l));
        assert_eq!(by(l), vec![Some("reach-rescue")], "{l}");
    }
    // --- the system-level properties (cross-block: FSM → core → register block) ---
    for l in [
        "edn_main_sm_error_is_terminal",
        "edn_main_sm_error_is_recorded",
        "edn_main_sm_error_record_is_sticky",
        "edn_ack_sm_0_error_is_terminal",
        "edn_ack_sm_0_error_is_recorded",
        "edn_no_deadlock",
        // the entry-condition form; the invariant form ("in SWPortMode the boot-request control
        // is off") is FALSE — software may rewrite CTRL while the FSM stays in SWPortMode
        "edn_swport_entry_excludes_boot_mode",
        // all eight ERR_CODE bits; decided by the reach rescue's k-induction in half a second
        "edn_fatal_alert_has_a_source",
    ] {
        assert_eq!(rows(l).len(), 1, "{l}");
        assert!(holds(l), "{l}: {:?}", rows(l));
    }
    // The `EF` twins decide on their dual through the reach rescue (2026-10-18): each target is
    // reached from reset (SymbiYosys `cover` reaches the same within six cycles).
    for l in [
        "edn_main_sm_error_reachable",
        "edn_main_sm_swport_reachable",
        "edn_ack_sm_0_datawait_reachable",
        "edn_ack_sm_7_datawait_reachable",
    ] {
        assert!(holds(l), "{l}: {:?}", rows(l));
    }
    // The `AG EF Idle` rows are FALSE under `rst_ni=1`. Each FSM's SEC_CM `Error` state is reachable
    // (a software ERR_CODE_TEST write raises `fatal_loc_events`, which the main FSM's and every
    // endpoint FSM's `local_escalate_i` read) and terminal (the `*_error_is_terminal` rows above), so
    // from `Error` the FSM never returns to `Idle`. The trap certificate refutes them (ledger §3.18,
    // 2026-10-21) with the path into `Error`; until then they were pinned ⊥ — the state's cone is over
    // the exact cap and the cube certified no MUST path into the trap.
    for (l, trap) in [
        (
            "edn_main_sm_recover_idle",
            "u_edn_core.u_edn_main_sm.state_q == 382",
        ),
        (
            "edn_ack_sm_0_recover_idle",
            "u_edn_core.gen_ep_blk$0.u_edn_ack_sm_ep.state_q == 465",
        ),
        (
            "edn_ack_sm_7_recover_idle",
            "u_edn_core.gen_ep_blk$7.u_edn_ack_sm_ep.state_q == 465",
        ),
    ] {
        let p = report
            .properties
            .iter()
            .find(|p| p.label.as_deref() == Some(l))
            .unwrap_or_else(|| panic!("{l}: no row"));
        assert!(
            matches!(p.outcome, VerifyOutcome::Violated { .. }),
            "{l}: {:?}",
            p.outcome
        );
        assert_eq!(p.decided_by.as_deref(), Some("trap-certificate"), "{l}");
        let want = format!("trap: {trap}");
        assert!(
            report.notes.iter().any(|n| n.kind == "trap-certificate"
                && n.property.as_deref() == Some(p.name.as_str())
                && n.items.contains(&want)),
            "{l}: the certificate note names `{trap}`"
        );
        // The trace names state CELLS, not the `state_q` alias: the main FSM's 9-bit state shows as
        // `u_edn_core.edn_main_sm_state`, an endpoint FSM's as a cell inside its `u_state_regs`. The
        // last state holds the trap value in a 9-bit cell of the core (2026-10-21 measurement).
        let val: u64 = trap
            .rsplit(" == ")
            .next()
            .and_then(|v| v.parse().ok())
            .expect("a value");
        let last = p
            .counterexample
            .as_ref()
            .and_then(|cx| cx.prefix.last())
            .unwrap_or_else(|| panic!("{l}: the path into the trap"));
        assert!(
            last.iter().any(|(k, v)| k.starts_with("u_edn_core.")
                && v.bits() == 9
                && v.as_u64() == Some(val)),
            "{l}: the path ends in the trap: {last:?}"
        );
    }
    // Floors (2026-10-21): 44 own + 8 system + 4 `EF` twins HOLDS; 8 + 3 VIOLATED; nothing ⊥.
    let n_holds = report
        .properties
        .iter()
        .filter(|p| matches!(p.outcome, VerifyOutcome::Holds))
        .count();
    assert!(n_holds >= 56, "holds = {n_holds}");
    let n_violated = report
        .properties
        .iter()
        .filter(|p| matches!(p.outcome, VerifyOutcome::Violated { .. }))
        .count();
    assert_eq!(n_violated, 11, "violated");
    assert!(
        report
            .properties
            .iter()
            .all(|p| !matches!(p.outcome, VerifyOutcome::Unknown { .. })),
        "no row is left ⊥"
    );
}

/// Diagnostic probe, not a gate: the reach portfolio under a 60-s budget (the rescue slice's
/// shape) on the BTOR2 file named by `MUNUNU_PROBE_BTOR2`, printing every member's verdict and
/// refusal — the test-binary twin of `mununu btor2 verify <file>`, for comparing the two contexts
/// on a dumped rescue monitor (`MUNUNU_RESCUE_MONITOR_DUMP`).
#[test]
#[ignore = "diagnostic; needs MUNUNU_PROBE_BTOR2 and the image's engines"]
fn probe_reach_portfolio_on_env_btor2() {
    use mununu_core::adapter::reach_portfolio::decide_reach_portfolio_parallel_with_timeout;
    use mununu_core::adapter::run_budget::{Budget, enter};
    let Some(path) = std::env::var_os("MUNUNU_PROBE_BTOR2") else {
        eprintln!("MUNUNU_PROBE_BTOR2 unset — nothing to probe");
        return;
    };
    let content = std::fs::read_to_string(&path).expect("read the probe BTOR2");
    let file = mununu_core::adapter::btor2::parser::parse(&content).expect("parse the probe BTOR2");
    let budget = Budget::with_ms(60_000);
    let _g = enter(&budget);
    let t0 = std::time::Instant::now();
    let out =
        decide_reach_portfolio_parallel_with_timeout(&file, std::time::Duration::from_secs(60));
    eprintln!(
        "PROBE verdict={:?} reachable_by={:?} unreachable_by={:?} refused={:?} elapsed={:.1}s RUST_LOG={:?}",
        out.verdict,
        out.reachable_by,
        out.unreachable_by,
        out.refused,
        t0.elapsed().as_secs_f64(),
        std::env::var("RUST_LOG").ok()
    );
}

/// The second FULL OpenTitan IP — `csrng` (`examples/verify/ip_opentitan_csrng`, lowRISC/opentitan
/// 558921c, 67 files), its AES cipher core black-boxed (`aes_cipher_core_bbox.sv`: the block's
/// outputs are free inputs, a declared over-approximation). The IP's own assertions through
/// `sv verify-auto` exactly as the example's `run.sh` does — the SVA extracted from the real
/// `prim_assert` macros, the lift under the generated shim (`INC_ASSERT` defined, the two
/// `_lift.sv` variants), `rst_ni` pinned inactive, the bus free. The three
/// `gen_aes_cipher_control_fsm_svas` rows reference the black box's hierarchy and are
/// `unsupported` by construction. The pins are the 2026-10-22 measurement (the README table); a
/// definite verdict that moves is a finding, not a re-baseline.
#[test]
#[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored --nocapture"]
fn e2e_opentitan_ip_csrng_full() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/verify/ip_opentitan_csrng/source");
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(f).display()))
    };
    let order: Vec<String> = read("sources.txt")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut sources: Vec<(String, String)> = vec![("csrng.sv".to_string(), read("csrng.sv"))];
    sources.extend(
        order
            .iter()
            .filter(|f| *f != "csrng.sv")
            .map(|f| (f.clone(), read(f))),
    );
    let yopts = YosysOptions {
        top: Some("csrng".to_string()),
        use_sv2v: true,
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    let opts = VerifyAutoOptions {
        config_values: [("rst_ni".to_string(), 1u64)].into_iter().collect(),
        portfolio: Some(PortfolioMode::Sequential),
        lift_overrides: ip_lift_overrides(&dir),
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let report = verify_auto(&sources, &yopts, &opts).expect("the full IP lifts and runs");
    eprintln!(
        "csrng full IP: {} properties, {} unsupported, {} registers, {:.0}s",
        report.properties.len(),
        report.unsupported.len(),
        report.diagnostics.state_register_count,
        t0.elapsed().as_secs_f64()
    );
    for p in &report.properties {
        let reason = p
            .bottom_reason
            .as_ref()
            .map(|r| format!(" [{}]", r.one_line()))
            .unwrap_or_default();
        eprintln!(
            "  {:<44} {:<10} {}{reason}",
            p.label.as_deref().unwrap_or(&p.name),
            p.outcome.label(),
            p.decided_by.as_deref().unwrap_or("-")
        );
    }
    for (name, reason) in &report.unsupported {
        eprintln!("  unsupported {name}: {reason}");
    }
    // Exactly the one declared cut.
    assert_eq!(
        report.diagnostics.blackboxed_modules,
        vec!["aes_cipher_core".to_string()]
    );
    // The lift: `--frontend auto`'s read_verilog + sv2v attempt fails on sv2v's output, the
    // slang attempt dies in the BTOR backend's cell-level topological sort (yosys-slang's
    // `$buf` cells) and succeeds on the retry with `opt_clean` — this IP is the retry's e2e.
    assert_eq!(
        report.diagnostics.frontend.as_deref(),
        Some("yosys-slang (read_slang)")
    );
    assert!(
        report
            .diagnostics
            .lift_retry
            .as_deref()
            .is_some_and(|r| r.contains("topological loop")),
        "{:?}",
        report.diagnostics.lift_retry
    );
    assert!(
        report.notes.iter().any(|n| n.kind == "lift-retry"),
        "the retry is reported as a note"
    );
    assert!(
        report.diagnostics.state_register_count >= 200,
        "{}",
        report.diagnostics.state_register_count
    );
    assert!(
        report
            .unsupported
            .iter()
            .all(|(n, _)| n.contains("gen_aes_cipher_control_fsm_svas")),
        "only the black box's own FSM rows may be unsupported; got {:?}",
        report.unsupported
    );
    assert_eq!(report.unsupported.len(), 3, "{:?}", report.unsupported);
    // --- the pins: (label, rows, outcome) ---
    let count = |label: &str, outcome: &str| {
        report
            .properties
            .iter()
            .filter(|p| p.label.as_deref() == Some(label) && p.outcome.label() == outcome)
            .count()
    };
    let total = |label: &str| {
        report
            .properties
            .iter()
            .filter(|p| p.label.as_deref() == Some(label))
            .count()
    };
    for (label, rows, outcome) in CSRNG_PINS {
        assert_eq!(
            total(label),
            *rows,
            "{label}: expected {rows} row(s), got {}",
            total(label)
        );
        assert_eq!(
            count(label, outcome),
            *rows,
            "{label}: expected every row {outcome}; got {:?}",
            report
                .properties
                .iter()
                .filter(|p| p.label.as_deref() == Some(label))
                .map(|p| (p.outcome.label(), p.decided_by.clone()))
                .collect::<Vec<_>>()
        );
    }
    let pinned: usize = CSRNG_PINS.iter().map(|(_, n, _)| n).sum();
    assert_eq!(
        pinned,
        report.properties.len(),
        "every row is pinned; unpinned: {:?}",
        report
            .properties
            .iter()
            .filter(|p| !CSRNG_PINS
                .iter()
                .any(|(l, _, _)| p.label.as_deref() == Some(l)))
            .map(|p| p.label.clone())
            .collect::<Vec<_>>()
    );
}

/// `e2e_opentitan_ip_csrng_full`'s pins — the 2026-10-22 measurement, one entry per assertion
/// label: (label, number of rows, the outcome every row decides).
const CSRNG_PINS: &[(&str, usize, &str)] = &[
    // `ASSERT_KNOWN`: 20 rows fold to a constant (2-state model), the four
    // `CsrngGenbits{Fips,Bus}KnownO_A` rows carry an enable and decide on the `explicit` cube.
    ("TlDValidKnownO_A", 1, "holds"),
    ("TlAReadyKnownO_A", 1, "holds"),
    ("EsReqKnownO_A", 1, "holds"),
    ("CsrngReqReadyKnownO_A", 2, "holds"),
    ("CsrngRspAckKnownO_A", 2, "holds"),
    ("CsrngRspStsKnownO_A", 2, "holds"),
    ("CsrngGenbitsValidKnownO_A", 2, "holds"),
    ("CsrngGenbitsFipsKnownO_AKnownEnable", 2, "holds"),
    ("CsrngGenbitsFipsKnownO_A", 2, "holds"),
    ("CsrngGenbitsBusKnownO_AKnownEnable", 2, "holds"),
    ("CsrngGenbitsBusKnownO_A", 2, "holds"),
    ("AlertTxKnownO_A", 1, "holds"),
    ("IntrCsCmdReqDoneKnownO_A", 1, "holds"),
    ("IntrCsEntropyReqKnownO_A", 1, "holds"),
    ("IntrCsHwInstExcKnownO_A", 1, "holds"),
    ("IntrCsFatalErrKnownO_A", 1, "holds"),
    // error → alert: the four `prim_count` chains through the reach rescue (bounded-chain
    // monitor + rIC3), the five FSM rows and the one-hot row on the `explicit` cube. The FSM
    // rows are vacuous on this model (`unused_err_o` is the synthesis constant, README).
    ("FpvSecCmCtrDrbgGenAlertCheck_A", 1, "holds"),
    ("FpvSecCmCntAlertCheck_A", 3, "holds"),
    ("FpvSecCmDrbgCmdFsmCheck_A", 3, "holds"),
    ("FpvSecCmCtrlMainFsmCheck_A", 1, "holds"),
    ("FpvSecCmDrbgGenFsmCheck_A", 1, "holds"),
    ("FpvSecCmRegWeOnehotCheck_A", 1, "holds"),
];

/// PR A1 — the fault-injection posture on the `edn` IP's own security-countermeasure checks
/// (`ASSERT_PRIM_*_ERROR_TRIGGER_ALERT` ×15): first the "before" instrument (`sec-cm:0`,
/// selection + the antecedent-vacuity check, nothing injected), then the posture proper
/// (`sec-cm`, one injection after a 10-cycle warm-up, the alert receiver pinned to its idle
/// encoding `alert_rx_i=85` so the sender's `sigint` logic does not force `alert_p` low).
///
/// Predictions, with their hypothesis-false readings in the ledger (§3.23): on the un-faulted
/// model the nine FSM rows are vacuous (`unused_err_o` is a synthesis constant, PR A0) and the
/// five counter chains + the one-hot row are not (`err_o` is real logic); under the posture the
/// nine FSM rows become non-vacuous (a corrupted state takes the `default:` arm → Error →
/// `*_sm_err` → the fatal alert) and every one of the fifteen HOLDS. The rows are printed
/// before the pins, so a wrong prediction is diagnosable from the transcript.
#[test]
#[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored --nocapture"]
fn e2e_opentitan_ip_edn_sec_cm_posture() {
    use mununu_core::adapter::btor2::fault::{FaultBound, FaultPosture, FaultSelection};
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/verify/ip_opentitan_edn/source");
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(f).display()))
    };
    let order: Vec<String> = read("sources.txt")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut sources: Vec<(String, String)> = vec![("edn.sv".to_string(), read("edn.sv"))];
    sources.extend(
        order
            .iter()
            .filter(|f| *f != "edn.sv")
            .map(|f| (f.clone(), read(f))),
    );
    let yopts = YosysOptions {
        top: Some("edn".to_string()),
        use_sv2v: true,
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    let run = |bound: FaultBound, pin_rx: bool| {
        let mut config: std::collections::HashMap<String, u64> =
            [("rst_ni".to_string(), 1u64)].into_iter().collect();
        if pin_rx {
            config.insert("alert_rx_i".to_string(), 85);
        }
        let opts = VerifyAutoOptions {
            config_values: config,
            portfolio: Some(PortfolioMode::Sequential),
            lift_overrides: ip_lift_overrides(&dir),
            faults: Some(FaultPosture {
                selection: FaultSelection::SecCm(bound),
                warmup: 10,
            }),
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let report = verify_auto(&sources, &yopts, &opts).expect("the IP lifts and runs");
        eprintln!(
            "edn sec-cm posture {:?}: {} properties, {} sites, {:.0}s — {}",
            bound,
            report.properties.len(),
            report.diagnostics.fault_sites.len(),
            t0.elapsed().as_secs_f64(),
            report.diagnostics.fault_model.as_deref().unwrap_or("-")
        );
        for s in &report.diagnostics.fault_sites {
            eprintln!(
                "  site {:<60} {:>3} b {:<16} {:?}",
                s.register, s.width, s.selected_by, s.rows
            );
        }
        for p in &report.properties {
            let label = p.label.as_deref().unwrap_or(&p.name);
            if !label.starts_with("FpvSecCm") {
                continue;
            }
            eprintln!(
                "  {:<34} {:<9} {:<16} vacuity={}",
                label,
                p.outcome.label(),
                p.decided_by.as_deref().unwrap_or("-"),
                p.vacuity.as_ref().map(|v| v.kind.tag()).unwrap_or("-")
            );
        }
        for n in report
            .notes
            .iter()
            .filter(|n| n.kind.starts_with("antecedent-"))
        {
            eprintln!("  note {}: {}", n.kind, n.summary);
        }
        report
    };
    fn sec_cm(
        r: &mununu_core::adapter::slang::verify_auto::AutoVerifyReport,
    ) -> Vec<&mununu_core::adapter::slang::verify_auto::PropertyVerdict> {
        r.properties
            .iter()
            .filter(|p| {
                p.label
                    .as_deref()
                    .is_some_and(|l| l.starts_with("FpvSecCm"))
            })
            .collect()
    }
    // --- the instrument: sec-cm:0 ---
    let before = run(FaultBound::AtMost(0), false);
    let rows = sec_cm(&before);
    assert_eq!(rows.len(), 15, "the fifteen error → alert chains");
    // 10 counter copies (`instance-prefix`), the FSM state cells (`fsm-mint`: the lift's flag is a
    // synthesis constant, re-minted from the state's compared constants), the cells the one-hot
    // flag's cone reaches (`cone-fallback`) — measured 2026-10-23
    assert!(
        before.diagnostics.fault_sites.len() >= 19,
        "{:?}",
        before.diagnostics.fault_sites
    );
    assert!(
        before
            .diagnostics
            .fault_sites
            .iter()
            .any(|s| s.selected_by == "fsm-mint"),
        "{:?}",
        before.diagnostics.fault_sites
    );
    let vacuous: Vec<&str> = rows
        .iter()
        .filter(|p| {
            p.vacuity
                .as_ref()
                .is_some_and(|v| v.kind.tag() == "antecedent-unreachable")
        })
        .map(|p| p.label.as_deref().unwrap_or(&p.name))
        .collect();
    // On the un-faulted model EVERY chain is vacuous: no fault flag can rise — the FSM flags
    // because the state never leaves its encoding, the counter flags because the duplicated
    // copies never disagree (measured 2026-10-23, native k-induction).
    assert_eq!(vacuous.len(), 15, "{vacuous:?}");
    // --- the posture: sec-cm, one injection, the receiver pinned ---
    let after = run(FaultBound::AtMost(1), true);
    let rows = sec_cm(&after);
    assert_eq!(rows.len(), 15);
    assert!(!after.diagnostics.fault_sites.is_empty());
    // Measured 2026-10-23 (2,754 s): NO countermeasure is escaped under one fault — no row is
    // violated; the rows that decide hold, several with a reachable antecedent (the response is
    // exercised); the rest stay ⊥ within the budget (each site widens the cone).
    let violated: Vec<&str> = rows
        .iter()
        .filter(|p| matches!(p.outcome, VerifyOutcome::Violated { .. }))
        .map(|p| p.label.as_deref().unwrap_or(&p.name))
        .collect();
    assert!(
        violated.is_empty(),
        "an escape under one fault: {violated:?}"
    );
    let holds = rows
        .iter()
        .filter(|p| matches!(p.outcome, VerifyOutcome::Holds))
        .count();
    assert!(holds >= 10, "{holds} of 15 hold");
    let reachable = after
        .notes
        .iter()
        .filter(|n| n.kind == "antecedent-reachable")
        .count();
    assert!(reachable >= 6, "{reachable} rows shown non-vacuous");
    // The one-hot row's flag reads the register-write enables, not state: its `cone-fallback`
    // sites cannot raise it, so it stays vacuous — a site-selection limit, recorded, not hidden.
    assert!(
        rows.iter()
            .filter(|p| p.label.as_deref() == Some("FpvSecCmRegWeOnehotCheck_A"))
            .all(|p| p
                .vacuity
                .as_ref()
                .is_some_and(|v| v.kind.tag() == "antecedent-unreachable")),
        "the one-hot row's vacuity is the documented limit"
    );
}

/// The full-IP system properties are not invariants in disguise: freezing the sticky `ERR_CODE`
/// record (`stick:u_reg.u_err_code_edn_main_sm_err.q`, the model's alias of the register) flips
/// `edn_main_sm_error_is_recorded` HOLDS → VIOLATED — the main FSM still reaches `Error` (software
/// error injection through the free bus) and the record now never follows — while
/// `edn_main_sm_error_is_terminal`, which does not read the record, stays HOLDS. Same sources and
/// lift as [`e2e_opentitan_ip_edn_full`], the two properties carried inline so the baseline and the
/// mutant each run only the IP's own SVA beside them.
#[test]
#[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored --nocapture"]
fn e2e_opentitan_ip_edn_stick_flips_the_error_record() {
    use mununu_core::adapter::btor2::mutate::Mutation;
    use mununu_core::adapter::slang::verify_auto::mutate_and_compare;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/verify/ip_opentitan_edn/source");
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(f).display()))
    };
    let order: Vec<String> = read("sources.txt")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut sources: Vec<(String, String)> = vec![("edn.sv".to_string(), read("edn.sv"))];
    sources.extend(
        order
            .iter()
            .filter(|f| *f != "edn.sv")
            .map(|f| (f.clone(), read(f))),
    );
    sources.push((
        "mununu_flip_props.sv".to_string(),
        "package mununu_flip_props; endpackage\n\
         // @mununu_guarantee(edn_main_sm_error_is_terminal) nu X. ((!(u_edn_core.u_edn_main_sm.state_q == 382) || nu Y. ((u_edn_core.u_edn_main_sm.state_q == 382) && [] Y)) && [] X)\n\
         // @mununu_guarantee(edn_main_sm_error_is_recorded) nu X. ((!(u_edn_core.u_edn_main_sm.state_q == 382) || [] u_reg.u_err_code_edn_main_sm_err.q) && [] X)\n"
            .to_string(),
    ));
    let yopts = YosysOptions {
        top: Some("edn".to_string()),
        use_sv2v: true,
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    let base_opts = VerifyAutoOptions {
        config_values: [("rst_ni".to_string(), 1u64)].into_iter().collect(),
        portfolio: Some(PortfolioMode::Sequential),
        lift_overrides: ip_lift_overrides(&dir),
        ..Default::default()
    };
    let report = mutate_and_compare(
        &sources,
        &yopts,
        &base_opts,
        Mutation::Stick("u_reg.u_err_code_edn_main_sm_err.q".into()),
    )
    .expect("baseline + mutant lifts of the full IP");
    for p in &report.properties {
        eprintln!(
            "  {:<40} {:<9} -> {:<9} {}",
            p.name,
            p.baseline,
            p.mutant,
            if p.flipped { "FLIP" } else { "" }
        );
    }
    // The flip report carries the positional name only: the inline file's first annotation is the
    // terminality row, its second the record row.
    let row = |name: &str| {
        report
            .properties
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("{name} in the mutation report"))
    };
    let recorded = row("ann_guarantee_1");
    assert_eq!(recorded.baseline, "holds", "{recorded:?}");
    assert!(
        recorded.flipped && recorded.mutant == "violated",
        "freezing the ERR_CODE record must flip `error_is_recorded` holds→violated: {recorded:?}"
    );
    let terminal = row("ann_guarantee_0");
    assert_eq!(terminal.baseline, "holds");
    assert!(
        !terminal.flipped,
        "the trap's terminality does not read the record: {terminal:?}"
    );
}
