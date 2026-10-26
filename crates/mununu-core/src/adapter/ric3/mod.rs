//! rIC3 subprocess model checker — the third external portfolio member (after
//! [`crate::adapter::btormc`] and [`crate::adapter::pono`]): a **bit-level IC3/PDR** engine that
//! reads BTOR2 directly (`rIC3 -e ic3 <file.btor2>`), ships in the `mununu-sva` image
//! (oss-cad-suite) and is subprocess-integrated at arm's length (MIT licence; no linkage).
//!
//! # Why a third member (Plan 4 / PR D2, 2026-10-19)
//!
//! The nine duplicated-counter fault-flag chains of OpenTitan `edn` / `csrng`
//! (`$rose(prim_count.err_o) |-> ##[0:30] alert_p`) need the inductive invariant "the two counter
//! copies agree": native k-induction cannot find it from a free step state, z3 SPACER does not find
//! it within the rescue slice, and the two existing subprocess members never parsed the design
//! (mununu#736). Measured on the rescue's dumped monitors before this member was written: rIC3
//! `-e ic3` proves all nine UNSAT in 1–14 s and agrees with mununu's own verdict on the 24 decided
//! control monitors (bus rows UNSAT, `EdnFatalAlertNoRsp_A` SAT) — 0 disagreements; SymbiYosys
//! `abc pdr` agrees.
//!
//! # Tool contract — `rIC3 -e ic3 <file.btor2>` (probed 2026-10-19, `mununu-sva`)
//!
//! | stdout (last token) | exit | meaning | [`McVerdict`] |
//! |---|---|---|---|
//! | `SAT`   | 10 | `bad` reachable (a real counterexample) | [`McVerdict::Violated`] |
//! | `UNSAT` | 20 | an inductive invariant: `bad` unreachable | [`McVerdict::Safe`] |
//! | anything else | 0 / other | undecided, or a tool error | [`McVerdict::Unknown`] |
//!
//! The exit codes follow the HWMCC convention (10 = unsafe, 20 = safe) — they are NON-zero on a
//! verdict, so, unlike pono's rule, a non-zero exit is an error only when it is none of {0, 10, 20}.
//! rIC3's default `portfolio` mode forks ~11 engines; the member runs the single `ic3` engine so
//! the reach portfolio's own parallelism stays bounded. rIC3 checks ONE property (the first `bad`):
//! on a design with more than one `bad` an `UNSAT` is a partial proof and is downgraded to
//! `Unknown`, as the other subprocess members do ([`count_bad_properties`]). No native wall clock:
//! [`crate::adapter::run_with_timeout_cancellable`] bounds and cancels it.
//!
//! # Trust
//!
//! The same class as btormc / pono: it reads the emitted BTOR2 (not mununu's CHC encoding, which is
//! why a lone SPACER `reachable` is distrusted) and returns a certificate-capable verdict. Promoted
//! to a decider after the HWMCC adjudication (`hwmcc_adjudication_over_user_dir`) and
//! `portfolio_agrees_across_engines_in_sva` ran with no contradiction — recorded on PR D2.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::adapter::btormc::{McVerdict, count_bad_properties};
use crate::adapter::{AdapterError, AdapterErrorKind};

/// The single engine the member runs (`rIC3 -e ic3`): IC3/PDR over the bit-level transition
/// system. The default `portfolio` mode forks ~11 solver processes and is not used here.
pub const DEFAULT_ENGINE: &str = "ic3";

/// Default wall-clock budget for a single rIC3 run (the reach portfolio's subprocess wall).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A discovered `rIC3` binary.
#[derive(Debug, Clone)]
pub struct Ric3Bin {
    pub path: PathBuf,
}

/// Locate a usable `rIC3` binary: `MUNUNU_RIC3_PATH`, else `rIC3` on `$PATH` (oss-cad-suite).
/// Presence is probed by invocability (`rIC3 --help`, any exit code); only a spawn failure is an
/// `Err` — the portfolio member is then simply unavailable, like btormc / pono.
pub fn locate_ric3() -> Result<Ric3Bin, AdapterError> {
    let path = std::env::var("MUNUNU_RIC3_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("rIC3"));
    match Command::new(&path).arg("--help").output() {
        Ok(_) => Ok(Ric3Bin { path }),
        Err(e) => Err(err(format!(
            "`{}` not invokable: {e}. Set MUNUNU_RIC3_PATH or install rIC3 (oss-cad-suite bundles it).",
            path.display()
        ))),
    }
}

/// Map rIC3's stdout to a verdict: the LAST `SAT` / `UNSAT` token wins (the engine logs before
/// it answers; the answer is the final line).
pub fn parse_ric3_output(stdout: &str) -> McVerdict {
    for line in stdout.lines().rev() {
        match line.trim() {
            "SAT" => return McVerdict::Violated,
            "UNSAT" => return McVerdict::Safe,
            _ => {}
        }
    }
    McVerdict::Unknown
}

/// rIC3's HWMCC exit codes: 10 = unsafe, 20 = safe, 0 = undecided; anything else is an error.
pub fn exit_code_is_a_verdict_or_clean(code: Option<i32>) -> bool {
    matches!(code, Some(0 | 10 | 20))
}

/// Run `rIC3 -e <engine> <file>` on a BTOR2 description and parse the verdict.
pub fn run_ric3_cancellable(
    bin: &Ric3Bin,
    btor2: &str,
    engine: &str,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<McVerdict, AdapterError> {
    run_ric3_with_output(bin, btor2, engine, false, timeout, cancel).map(|(v, _)| v)
}

/// [`run_ric3_cancellable`] with rIC3's stdout kept — and `--witness` asked for when `witness` —
/// for the witness path ([`crate::adapter::btor2::witness`], mununu#744). A timeout or a
/// cancellation is `(Unknown, "")`, never a wrong verdict.
pub fn run_ric3_with_output(
    bin: &Ric3Bin,
    btor2: &str,
    engine: &str,
    witness: bool,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(McVerdict, String), AdapterError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // rIC3 reads a FILE and picks the format from the suffix: `.btor2`.
    let mut tmp_path = std::env::temp_dir();
    tmp_path.push(format!(
        "mununu-ric3-{}-{}.btor2",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp_path, btor2).map_err(|e| {
        err(format!(
            "failed writing BTOR2 to {}: {e}",
            tmp_path.display()
        ))
    })?;
    let mut command = Command::new(&bin.path);
    command.arg("-e").arg(engine);
    if witness {
        command.arg("--witness");
    }
    command.arg(&tmp_path);
    let result = crate::adapter::run_with_timeout_cancellable(&mut command, None, timeout, cancel);
    let _ = std::fs::remove_file(&tmp_path);
    let outcome =
        result.map_err(|e| err(format!("failed to run `{}`: {e}", bin.path.display())))?;
    // Timed out or cancelled ⇒ inconclusive (never a wrong verdict).
    let Some((status, stdout, stderr)) = outcome else {
        return Ok((McVerdict::Unknown, String::new()));
    };
    // MULTI-PROPERTY SOUNDNESS — rIC3 checks the first `bad`; an `UNSAT` on a design with more
    // than one is a partial proof.
    let bad_count = count_bad_properties(btor2);
    let verdict = match parse_ric3_output(&stdout) {
        McVerdict::Safe if bad_count > 1 => McVerdict::Unknown,
        v => v,
    };
    // A verdict exits 10 / 20 and a clean undecided exits 0; anything else with no verdict is a
    // tool error (a parse rejection, a missing solver), surfaced so the portfolio can name it.
    if verdict == McVerdict::Unknown && !exit_code_is_a_verdict_or_clean(status.code()) {
        return Err(err(format!(
            "`{}` exited with status {} and no verdict; stderr: {}",
            bin.path.display(),
            status,
            stderr.lines().last().unwrap_or("").trim()
        )));
    }
    Ok((verdict, stdout))
}

/// The L2-seam → rIC3 decide path: emit the (possibly reduced / monitored) design for a subprocess
/// member and decide it with rIC3's IC3.
pub fn decide_via_ric3(
    file: &crate::adapter::btor2::ast::Btor2File,
    engine: &str,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<McVerdict, AdapterError> {
    let btor2 = crate::adapter::btor2::emit::emit_btor2_for_subprocess(file);
    let bin = locate_ric3()?;
    run_ric3_cancellable(&bin, &btor2, engine, timeout, cancel)
}

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!("adapter/ric3: {message}"),
        location: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    const REACH: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n6 add 1 3 5\n\
                         7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n";
    const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 s\n4 init 1 3 2\n5 next 1 3 3\n\
                        6 one 1\n7 eq 1 3 6\n8 bad 7\n";

    #[test]
    fn parse_ric3_output_maps_tokens() {
        assert_eq!(
            parse_ric3_output("[INFO ] the model to be checked: x\nSAT\n"),
            McVerdict::Violated
        );
        assert_eq!(parse_ric3_output("UNSAT\n"), McVerdict::Safe);
        assert_eq!(
            parse_ric3_output("[INFO ] preprocessed\n"),
            McVerdict::Unknown
        );
        assert_eq!(parse_ric3_output(""), McVerdict::Unknown);
    }

    /// rIC3 answers with the HWMCC exit codes, which are non-zero: 10 and 20 are verdicts, not
    /// errors; 0 is a clean undecided; anything else is a tool error.
    #[test]
    fn ric3_exit_codes_10_and_20_are_verdicts_not_errors() {
        assert!(exit_code_is_a_verdict_or_clean(Some(10)));
        assert!(exit_code_is_a_verdict_or_clean(Some(20)));
        assert!(exit_code_is_a_verdict_or_clean(Some(0)));
        assert!(!exit_code_is_a_verdict_or_clean(Some(1)));
        assert!(!exit_code_is_a_verdict_or_clean(Some(101)));
        assert!(!exit_code_is_a_verdict_or_clean(None));
    }

    #[test]
    fn locate_absent_ric3_is_graceful_error() {
        let _ = locate_ric3();
    }

    #[test]
    #[ignore = "requires rIC3 (MUNUNU_RIC3_PATH or $PATH); run with --ignored in mununu-sva"]
    fn decide_via_ric3_emits_and_decides() {
        let reach = parser::parse(REACH).expect("parse reach");
        assert_eq!(
            decide_via_ric3(&reach, DEFAULT_ENGINE, DEFAULT_TIMEOUT, None).unwrap(),
            McVerdict::Violated
        );
        let safe = parser::parse(SAFE).expect("parse safe");
        assert_eq!(
            decide_via_ric3(&safe, DEFAULT_ENGINE, DEFAULT_TIMEOUT, None).unwrap(),
            McVerdict::Safe
        );
    }
}
