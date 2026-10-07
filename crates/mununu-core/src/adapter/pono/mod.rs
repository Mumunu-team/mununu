//! Pono subprocess model checker — the second external portfolio member
//! (after [`crate::adapter::btormc`]), and the P1 "borrowed" scalable-safety track.
//!
//! Pono (Stanford, **BSD-3** — subprocess-integrated at arm's length, so zero
//! license poisoning) brings **IC3/PDR** proof engines that btormc's
//! BMC + k-induction lacks. Neither engine dominates: on the btor2tools suite
//! btormc's BMC finds a deep counterexample (`count4`/`recount4`) that Pono's
//! IC3 leaves `unknown`, while Pono's IC3 decides a violation (`factorial4even`)
//! that btormc's multi-property `--kind` cannot. So the portfolio btormc ⊕ Pono
//! is strictly stronger than either — the P1 rationale.
//!
//! # Tool — `pono -e <engine> <file>`
//!
//! Pono reads a BTOR2 **file** (not stdin) and prints its verdict on the first
//! stdout line. Probed contract (2026-07-07, `mununu-sva` oss-cad-suite build):
//!
//! | pono stdout (first token) | meaning | [`McVerdict`] |
//! |---|---|---|
//! | `sat`     | a reachable `bad` (a real counterexample) | [`McVerdict::Violated`] |
//! | `unsat`   | the engine PROVED `bad` unreachable        | [`McVerdict::Safe`]     |
//! | `unknown` | bounded / gave up — no CEX and no proof    | [`McVerdict::Unknown`]  |
//!
//! Engines: `bmc`, `bmc-sp`, `ind`, `mbic3`, `ic3bits`, `ic3sa`, `sygus-pdr`
//! (default [`DEFAULT_ENGINE`] = `ic3bits`, a bit-level IC3 that proves safety
//! AND finds shallow CEXs). `interp` / `ic3ia` need a MathSAT build absent from
//! the image and are not selected.
//!
//! **Soundness.** `unsat` is printed only when a proof engine actually closes
//! (bounded runs print `unknown`), so `unsat` ⇒ a genuine unbounded safety proof.
//! Like btormc, Pono checks ONE property at a time (index 0 by default), so on a
//! design with more than one `bad` an `unsat` is only a PARTIAL proof —
//! [`run_pono`] downgrades a `Safe` parse to [`McVerdict::Unknown`] when
//! [`count_bad_properties`] > 1. A `sat` (some property reachable) stays a sound
//! [`McVerdict::Violated`].

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::adapter::btormc::{McVerdict, count_bad_properties};
use crate::adapter::{AdapterError, AdapterErrorKind};

/// Default Pono engine: bit-level IC3 (`ic3bits`) — the PDR proof engine that
/// gives Pono its differentiated value over btormc. Proves safety (`unsat`) and
/// finds shallow counterexamples (`sat`); deep-CEX designs are btormc's strength
/// in the portfolio.
pub const DEFAULT_ENGINE: &str = "ic3bits";

/// Default wall-clock budget for a single Pono run. Pono's IC3/PDR has **no
/// native wall-clock bound** (only depth caps on the bounded engines), so a hard
/// instance can run unbounded — the external kill in
/// [`crate::adapter::run_with_timeout`] is the only backstop. A timeout is a sound
/// abstention: it yields [`McVerdict::Unknown`], never a wrong verdict.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A discovered `pono` binary + its parsed version (diagnostic only).
#[derive(Debug, Clone)]
pub struct PonoBin {
    pub path: PathBuf,
    pub version: String,
}

/// Locate a usable `pono` binary. Discovery order:
/// 1. `MUNUNU_PONO_PATH` env var (explicit override).
/// 2. `pono` on `$PATH` (oss-cad-suite).
///
/// Unlike the shared [`crate::adapter::locate_tool`], Pono has **no version
/// flag** (`--version` / `-h` both just print USAGE, and exit-code is not a
/// reliable presence signal), so existence is probed by *invocability*: spawn
/// `pono -h` and accept ANY exit code — only a spawn failure (binary absent)
/// is an `Err`. Callers fall back gracefully (the portfolio member is simply
/// unavailable, exactly like btormc / cvc5).
pub fn locate_pono() -> Result<PonoBin, AdapterError> {
    let path = std::env::var("MUNUNU_PONO_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("pono"));
    match Command::new(&path).arg("-h").output() {
        Ok(_) => Ok(PonoBin {
            path,
            version: "<pono; no --version>".to_string(),
        }),
        Err(e) => Err(err(format!(
            "`{}` not invokable: {e}. Set MUNUNU_PONO_PATH or install pono (oss-cad-suite bundles it).",
            path.display()
        ))),
    }
}

/// Map Pono's stdout to a verdict: the first `sat` / `unsat` / `unknown` token wins.
pub fn parse_pono_output(stdout: &str) -> McVerdict {
    for line in stdout.lines() {
        match line.trim() {
            "sat" => return McVerdict::Violated,
            "unsat" => return McVerdict::Safe,
            "unknown" => return McVerdict::Unknown,
            _ => {}
        }
    }
    McVerdict::Unknown
}

/// Run `pono -e <engine> <file>` on a BTOR2 description and parse the verdict.
///
/// Pono reads a file, so `btor2` is written to a temp file for the run. The
/// multi-property `Safe`-downgrade mirrors [`crate::adapter::btormc::run_btormc`].
/// `Err` only when the subprocess could not run; a clean run with no `sat`/`unsat`
/// is [`McVerdict::Unknown`], not an error.
pub fn run_pono(
    bin: &PonoBin,
    btor2: &str,
    engine: &str,
    timeout: Duration,
) -> Result<McVerdict, AdapterError> {
    run_pono_cancellable(bin, btor2, engine, timeout, None)
}

/// [`run_pono`] with the reach portfolio's cooperative **cancel** flag: once another member
/// has decided, the child is killed after a short grace and the run reads as
/// [`McVerdict::Unknown`] — exactly like a timeout. See
/// [`crate::adapter::run_with_timeout_cancellable`].
pub fn run_pono_cancellable(
    bin: &PonoBin,
    btor2: &str,
    engine: &str,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<McVerdict, AdapterError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Pono reads a file (not stdin). Write to a uniquely-named temp file — the
    // codebase's `std::env::temp_dir` convention (as in the verilator wrapper),
    // not the dev-only `tempfile` crate. Best-effort cleanup regardless of outcome.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut tmp_path = std::env::temp_dir();
    tmp_path.push(format!(
        "mununu-pono-{}-{}.btor2",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp_path, btor2).map_err(|e| {
        err(format!(
            "failed writing BTOR2 to {}: {e}",
            tmp_path.display()
        ))
    })?;

    // Pono's IC3/PDR has no native wall-clock bound — enforce one externally so a
    // hard instance abstains (Unknown) rather than hanging the portfolio.
    let mut command = Command::new(&bin.path);
    command.arg("-e").arg(engine).arg(&tmp_path);
    let result = crate::adapter::run_with_timeout_cancellable(&mut command, None, timeout, cancel);
    let _ = std::fs::remove_file(&tmp_path);
    let outcome =
        result.map_err(|e| err(format!("failed to run `{}`: {e}", bin.path.display())))?;
    // Timed out ⇒ inconclusive (never a wrong verdict).
    let Some((status, stdout, stderr)) = outcome else {
        return Ok(McVerdict::Unknown);
    };

    // MULTI-PROPERTY SOUNDNESS — Pono checks property index 0 by default, so an
    // `unsat` on a design with >1 `bad` proves only that one property safe. Downgrade
    // a `Safe` parse to `Unknown`; a `sat` (some property reachable) stays `Violated`.
    let bad_count = count_bad_properties(btor2);
    let verdict = match parse_pono_output(&stdout) {
        McVerdict::Safe if bad_count > 1 => McVerdict::Unknown,
        v => v,
    };

    // A clean Unknown is valid; a hard error (non-zero exit, no verdict) is not —
    // surface it as a malformed-input signal rather than inconclusiveness.
    if verdict == McVerdict::Unknown && !status.success() {
        return Err(err(format!(
            "`{}` exited with status {} and no verdict; stderr: {}",
            bin.path.display(),
            status,
            stderr.trim()
        )));
    }
    Ok(verdict)
}

/// The L2-seam → Pono decide path: emit a (possibly reduced / transformed)
/// [`Btor2File`](crate::adapter::btor2::ast::Btor2File) and decide it with Pono's
/// IC3. The Pono analogue of [`crate::adapter::btormc::decide_via_btormc`].
pub fn decide_via_pono(
    file: &crate::adapter::btor2::ast::Btor2File,
    engine: &str,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<McVerdict, AdapterError> {
    let btor2 = crate::adapter::btor2::emit::emit_btor2(file);
    let bin = locate_pono()?;
    run_pono_cancellable(&bin, &btor2, engine, timeout, cancel)
}

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!("adapter/pono: {message}"),
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
    fn parse_pono_output_maps_tokens() {
        assert_eq!(parse_pono_output("sat\nb0\n"), McVerdict::Violated);
        assert_eq!(parse_pono_output("unsat\nb0\n"), McVerdict::Safe);
        assert_eq!(parse_pono_output("unknown\nb0\n"), McVerdict::Unknown);
        assert_eq!(parse_pono_output(""), McVerdict::Unknown);
    }

    #[test]
    fn locate_absent_pono_is_graceful_error() {
        // With MUNUNU_PONO_PATH pointing at nothing and pono off $PATH the locate
        // is a clean Err, never a panic. (When pono IS on $PATH this simply
        // succeeds — the point is no panic either way.)
        let _ = locate_pono();
    }

    #[test]
    #[ignore = "requires pono (MUNUNU_PONO_PATH or $PATH); run with --ignored in mununu-sva"]
    fn decide_via_pono_emits_and_decides() {
        // The L2-seam → Pono path end-to-end via `ic3bits` (IC3 proves both ways):
        // a reachable-bad model is Violated, a safe one is Safe.
        let reach = parser::parse(REACH).expect("parse reach");
        assert_eq!(
            decide_via_pono(&reach, DEFAULT_ENGINE, DEFAULT_TIMEOUT, None).unwrap(),
            McVerdict::Violated
        );
        let safe = parser::parse(SAFE).expect("parse safe");
        assert_eq!(
            decide_via_pono(&safe, DEFAULT_ENGINE, DEFAULT_TIMEOUT, None).unwrap(),
            McVerdict::Safe
        );
    }
}
