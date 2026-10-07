//! H.O.1a (2026-06-29) — external BTOR2 model-checker wrapper (`btormc`).
//!
//! # Why this module exists
//!
//! H.O.0 ([`crate::adapter::btor2::concrete_oracle`]) is the *internal* exact
//! oracle: it computes a safety property's concrete truth by **bounded
//! reachability enumeration** (mununu's own one-step semantics, no abstraction).
//! That is sound for finding violations always, but it can only conclude
//! `AG`-true when it enumerated the full input space — so on a real OpenTitan
//! design with wide config inputs (sysrst's `cfg_detect_timer_i`, …) it is forced
//! to [`AgOracle::Inconclusive`](crate::adapter::btor2::concrete_oracle::AgOracle::Inconclusive):
//! it can REFUTE a spurious cube-`HOLDS` but cannot CONFIRM a real-RTL `HOLDS`
//! (H.O.0.3b established exactly this boundary).
//!
//! H.O.1 closes that gap with an **external, symbolic** oracle: emit the
//! safety-fragment property as a BTOR2 `bad` monitor (H.O.1b) and let a real
//! BTOR2 model checker decide it. `btormc` works over SMT (no enumeration), so it
//! returns a *definite* verdict on the same wide-input designs the internal
//! oracle cannot.
//!
//! # Tool choice — `btormc --kind`
//!
//! The `mununu-sva` image's oss-cad-suite bundles `btormc`, `pono`, `bitwuzla`,
//! `boolector`. `pono`'s strongest engine (`ic3ia`) needs a MathSAT build that is
//! absent; `btormc` reads BTOR2 `bad` natively and proves unbounded safety via
//! k-induction (`--kind`). Probed contract (2026-06-29, in `mununu-sva`):
//!
//! | btormc stdout (first token) | meaning | [`McVerdict`] |
//! |---|---|---|
//! | `sat` | a reachable `bad` (with a witness trace) | [`McVerdict::Violated`] |
//! | `unsat` | k-induction CLOSED — `bad` unreachable (a real proof) | [`McVerdict::Safe`] |
//! | *(neither / empty)* | bounded: no CEX and no proof within `-kmax` | [`McVerdict::Unknown`] |
//!
//! **Soundness of the contract.** `unsat` is printed ONLY under `--kind` when
//! k-induction actually closes — plain BMC never prints it. The soundness-critical
//! case (a CEX at depth 5 run under `--kind -kmax 2`) printed NOTHING, not a false
//! `unsat`. So `unsat` ⇒ a genuine unbounded safety proof, and a too-shallow
//! `-kmax` degrades to [`McVerdict::Unknown`], never to a wrong [`McVerdict::Safe`].
//!
//! **Multi-property caveat (2026-07-06).** On a design with MORE THAN ONE `bad`,
//! `--kind` reports PER PROPERTY and may print `unsat bN` (property N safe) and STOP
//! while a DIFFERENT property is reachable — so a lone `unsat` line is only a PARTIAL
//! proof, not a whole-design safety proof. [`run_btormc`] therefore downgrades a
//! `Safe` parse to [`McVerdict::Unknown`] when [`count_bad_properties`] > 1 (a `sat`
//! — any property reachable — stays a sound [`McVerdict::Violated`]). Found by the
//! HWMCC coverage study on `factorial4even` (`unsat b1` while `b0 = i==15` reachable).
//!
//! # `btormc` is optional at runtime
//!
//! Like the CVC5 wrapper ([`crate::adapter::cvc5`]) — discovery returns a
//! structured [`AdapterError`] when the binary is absent; callers (the H.O.1c
//! differential / e2e) treat that as "oracle unavailable", never a hard failure.
//! Per the subprocess-tools-not-bundled policy, `btormc` is contributor-installed
//! (oss-cad-suite / Homebrew `yosys` formula) — it is NOT in `Dockerfile.dev`; the
//! `mununu-sva` image carries it for the `#[ignore]`-gated e2e validation.
//!
//! # H.O.1a scope
//!
//! THIS increment ships the wrapper only: binary discovery + version probe
//! ([`locate_btormc`]), the verdict enum ([`McVerdict`]), output parsing
//! ([`parse_btormc_output`], make-ci unit-tested against canned stdout), and the
//! subprocess invocation ([`run_btormc`], docker-validated). The `bad`-monitor
//! EMISSION (property → augmented BTOR2) is H.O.1b; the `McOracle` differential +
//! real-RTL e2e is H.O.1c.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::adapter::{AdapterError, AdapterErrorKind};

/// Default k-induction / BMC bound for [`run_btormc`]. Picked generous enough to
/// reach a CEX or close k-induction on the small FSM fragments H.O targets, while
/// bounding wall-clock on the e2e. Callers can override per-invocation.
pub const DEFAULT_KMAX: u32 = 40;

/// Default wall-clock timeout for a [`run_btormc`] invocation. `-kmax` bounds the
/// unroll DEPTH, but each SAT check on a wide design can still be slow; this caps
/// the total run so a portfolio member cannot hang the whole decision.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A discovered `btormc` binary + its parsed version (diagnostic only — mununu
/// does not gate on a minimum version).
#[derive(Debug, Clone)]
pub struct BtormcBin {
    /// Resolved path: a bare `btormc` (found on `$PATH`) or the absolute path
    /// from `MUNUNU_BTORMC_PATH`.
    pub path: PathBuf,
    /// Version string from `btormc --version` (e.g. `3.2.4`), or
    /// `"<unparseable>"` when the output does not match the expected shape.
    pub version: String,
}

/// The external model checker's verdict on a BTOR2 `bad` property. See the module
/// docs for the `btormc --kind` output contract this maps from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McVerdict {
    /// k-induction proved the `bad` unreachable — the property is SAFE (`HOLDS`).
    Safe,
    /// A reachable `bad` (a sound counterexample) — the property is VIOLATED.
    Violated,
    /// Neither a CEX nor a proof within `-kmax` — bounded, inconclusive.
    Unknown,
}

/// H.O.1a — locate a usable `btormc` binary and probe its version.
///
/// Discovery order (mirrors [`crate::adapter::cvc5::locate_cvc5`] /
/// [`crate::adapter::verilator::locate_verilator`]):
/// 1. `MUNUNU_BTORMC_PATH` env var (explicit override).
/// 2. `btormc` on `$PATH` (oss-cad-suite, Homebrew `yosys`, …).
///
/// Returns `Err(AdapterError { kind: UnsupportedConstruct, .. })` when the binary
/// is absent or the version probe fails — callers fall back gracefully (the
/// oracle is simply unavailable).
pub fn locate_btormc() -> Result<BtormcBin, AdapterError> {
    // AR-GO-2 — shared locate body; see `crate::adapter::locate_tool`.
    let (path, version) = crate::adapter::locate_tool(
        "MUNUNU_BTORMC_PATH",
        "btormc",
        "btormc",
        "Set MUNUNU_BTORMC_PATH or install btormc (oss-cad-suite, or the Homebrew \
         `yosys` formula which bundles the btor2tools).",
        parse_btormc_version,
    )?;
    Ok(BtormcBin { path, version })
}

/// Extract the version token from `btormc --version` output (observed: a single
/// line, e.g. `3.2.4`). Returns the first non-empty trimmed line, or `None` when
/// the output is empty — the caller treats that as `"<unparseable>"` (diagnostic
/// only; the binary may still run queries).
pub fn parse_btormc_version(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Map `btormc` stdout to an [`McVerdict`]. The first line that is exactly `sat`
/// or `unsat` decides; absence of both is [`McVerdict::Unknown`] (bounded run, no
/// CEX and no proof). See the module docs for why `unsat` is a sound `Safe`.
pub fn parse_btormc_output(stdout: &str) -> McVerdict {
    for line in stdout.lines() {
        match line.trim() {
            "sat" => return McVerdict::Violated,
            "unsat" => return McVerdict::Safe,
            _ => {}
        }
    }
    McVerdict::Unknown
}

/// Count the `bad` property lines in a BTOR2 text — a `<nid> bad <operand>` line
/// (the second whitespace token is `bad`, the first a numeric node id). Comment
/// (`;`) and other lines are ignored. Used to detect multi-property designs, where
/// a `btormc --kind` `Safe` parse is only a partial proof (see [`run_btormc`]).
pub fn count_bad_properties(btor2: &str) -> usize {
    btor2
        .lines()
        .filter(|line| {
            let mut toks = line.split_whitespace();
            toks.next().is_some_and(|t| t.parse::<u64>().is_ok()) && toks.next() == Some("bad")
        })
        .count()
}

/// H.O.1a — run `btormc --kind -kmax <kmax>` on a BTOR2 description (piped via
/// stdin) and parse the verdict.
///
/// `btor2` is the full BTOR2 text **including a `bad` line** (the safety monitor;
/// H.O.1b builds it). The `--kind` flag enables the k-induction proof that lets
/// the run conclude [`McVerdict::Safe`]; without it `btormc` only ever finds CEXs.
///
/// Returns `Err` only when the subprocess could not run or `btormc` reported a
/// hard error (e.g. a malformed BTOR2 — a bug in the emission, not an
/// inconclusive verdict) with no parseable verdict on stdout. A clean run with
/// neither `sat` nor `unsat` is [`McVerdict::Unknown`], not an error.
pub fn run_btormc(
    bin: &BtormcBin,
    btor2: &str,
    kmax: u32,
    timeout: Duration,
) -> Result<McVerdict, AdapterError> {
    run_btormc_cancellable(bin, btor2, kmax, timeout, None)
}

/// [`run_btormc`] with the reach portfolio's cooperative **cancel** flag: once another
/// member has decided, the child is killed after a short grace and the run reads as
/// [`McVerdict::Unknown`] — exactly like a timeout. See
/// [`crate::adapter::run_with_timeout_cancellable`].
pub fn run_btormc_cancellable(
    bin: &BtormcBin,
    btor2: &str,
    kmax: u32,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<McVerdict, AdapterError> {
    let mut command = Command::new(&bin.path);
    command.arg("--kind").arg("-kmax").arg(kmax.to_string());
    let outcome = crate::adapter::run_with_timeout_cancellable(
        &mut command,
        Some(btor2.as_bytes()),
        timeout,
        cancel,
    )
    .map_err(|e| AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!(
            "adapter/btormc: failed to run `{}`: {e}",
            bin.path.display()
        ),
        location: None,
    })?;
    // Timed out ⇒ inconclusive (never a wrong verdict).
    let Some((status, stdout, stderr)) = outcome else {
        return Ok(McVerdict::Unknown);
    };

    // MULTI-PROPERTY SOUNDNESS FIX — `--kind` reports per property and may STOP after
    // proving ONE property of a multi-`bad` design safe (printing `unsat bN`) while a
    // DIFFERENT property is reachable. `parse_btormc_output` returns on the first
    // `sat`/`unsat` line, so a partial `unsat bN` would wrongly conclude the whole
    // design SAFE. On a design with >1 `bad`, downgrade a `Safe` parse to `Unknown` —
    // a partial proof cannot establish every property safe. A `sat` (some property IS
    // reachable) stays a sound `Violated`. Single-`bad` designs are unaffected.
    let bad_count = count_bad_properties(btor2);
    let verdict = match parse_btormc_output(&stdout) {
        McVerdict::Safe if bad_count > 1 => McVerdict::Unknown,
        v => v,
    };

    // A clean Unknown (exit 0, empty stdout) is valid. But if btormc reported a
    // hard error (non-zero exit, e.g. a BTOR2 parse error) AND produced no
    // verdict, surface it — that signals a malformed monitor, not inconclusiveness.
    if verdict == McVerdict::Unknown && !status.success() {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "adapter/btormc: `{}` exited with status {} and no verdict; stderr: {}",
                bin.path.display(),
                status,
                stderr.trim()
            ),
            location: None,
        });
    }

    Ok(verdict)
}

/// The L2-seam → subprocess decide path: emit a (possibly reduced / transformed)
/// [`Btor2File`](crate::adapter::btor2::ast::Btor2File) to BTOR2 text and hand it
/// to `btormc --kind` for a scalable safety verdict.
///
/// This is what lets mununu decide a model beyond the exact BDD engine's 40-bit
/// cone cap: reduce internally, [`emit`](crate::adapter::btor2::emit::emit_btor2)
/// the reduced query, and let the external engine (which honours `constraint`,
/// free-init state, and wide datapaths natively) decide it. For now the hand-off
/// is RAW — the emitted model is the file as-is; cone-of-influence + reduction
/// passes layer on top later.
///
/// `Err` when `btormc` is unavailable (locate failure) or the run fails; callers
/// degrade to `Skipped` exactly as the direct [`run_btormc`] path does.
pub fn decide_via_btormc(
    file: &crate::adapter::btor2::ast::Btor2File,
    kmax: u32,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<McVerdict, AdapterError> {
    let btor2 = crate::adapter::btor2::emit::emit_btor2(file);
    let bin = locate_btormc()?;
    run_btormc_cancellable(&bin, &btor2, kmax, timeout, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_sat_is_violated() {
        // btormc prints the bad index + a witness trace after `sat`.
        let out = "sat\nb0\n@0\n@1\n.\n";
        assert_eq!(parse_btormc_output(out), McVerdict::Violated);
    }

    #[test]
    fn parse_output_unsat_is_safe() {
        let out = "unsat\nb0\n";
        assert_eq!(parse_btormc_output(out), McVerdict::Safe);
    }

    #[test]
    fn parse_output_empty_is_unknown() {
        // The probed bounded case: a CEX deeper than -kmax + k-induction not yet
        // closed prints nothing → Unknown (NOT a false Safe).
        assert_eq!(parse_btormc_output(""), McVerdict::Unknown);
        assert_eq!(parse_btormc_output("\n  \n"), McVerdict::Unknown);
    }

    #[test]
    fn parse_output_first_verdict_wins() {
        // Defensive: a stray later token never overrides the first real verdict.
        assert_eq!(parse_btormc_output("sat\nunsat\n"), McVerdict::Violated);
        assert_eq!(parse_btormc_output("unsat\nsat\n"), McVerdict::Safe);
    }

    #[test]
    fn count_bad_properties_counts_bad_lines_only() {
        // `<nid> bad <op>` lines count; comments, other ops, and blanks do not.
        let single = "1 sort bitvec 1\n2 state 1\n3 bad 2\n";
        assert_eq!(count_bad_properties(single), 1);
        let multi =
            "; comment\n1 sort bitvec 4\n2 state 1 f\n14 bad 13\n15 slice 12 3 0 0\n19 bad 18\n";
        assert_eq!(
            count_bad_properties(multi),
            2,
            "factorial4even-shape: two `bad`s"
        );
        assert_eq!(count_bad_properties("1 sort bitvec 1\n2 constraint 1\n"), 0);
    }

    #[test]
    fn parse_version_single_line() {
        assert_eq!(parse_btormc_version("3.2.4\n").as_deref(), Some("3.2.4"));
        assert_eq!(parse_btormc_version("  3.2.4  ").as_deref(), Some("3.2.4"));
        assert_eq!(parse_btormc_version(""), None);
    }

    #[test]
    fn locate_absent_btormc_is_graceful_error() {
        // A bogus override path must yield a structured error, never a panic —
        // callers fall back to "oracle unavailable".
        // SAFETY: single-threaded test; restored before returning.
        unsafe { std::env::set_var("MUNUNU_BTORMC_PATH", "/nonexistent/btormc-xyz") };
        let r = locate_btormc();
        unsafe { std::env::remove_var("MUNUNU_BTORMC_PATH") };
        let err = r.expect_err("absent binary must error");
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        assert!(err.message.contains("btormc"));
    }

    // ---- docker-validated (`mununu-sva`): exercise the real binary -----------
    // These need `btormc` on PATH (oss-cad-suite); run with `--ignored`.

    /// A reachable `bad`: `q` init 0, `next q = 1` → `bad = q` true at k=1.
    /// (`init sid state val` needs `state_nid > val_nid`, so the const is first.)
    const REACH_BTOR2: &str = "\
1 sort bitvec 1
2 zero 1
3 one 1
4 state 1 q
5 init 1 4 2
6 next 1 4 3
7 bad 4
";

    /// An inductively-safe `bad`: `q` init 0, `next q = 0` → `bad = q` never
    /// reachable; `--kind` proves it (the property invariant `q == 0` is
    /// 1-inductive).
    const SAFE_BTOR2: &str = "\
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 2
6 bad 3
";

    #[test]
    #[ignore = "requires btormc (MUNUNU_BTORMC_PATH or $PATH); run with --ignored in mununu-sva"]
    fn run_btormc_reach_is_violated() {
        let bin = locate_btormc().expect("btormc present");
        assert_eq!(
            run_btormc(&bin, REACH_BTOR2, DEFAULT_KMAX, DEFAULT_TIMEOUT).unwrap(),
            McVerdict::Violated
        );
    }

    #[test]
    #[ignore = "requires btormc (MUNUNU_BTORMC_PATH or $PATH); run with --ignored in mununu-sva"]
    fn run_btormc_safe_is_safe() {
        let bin = locate_btormc().expect("btormc present");
        // `--kind` (set by run_btormc) is what lets this conclude Safe.
        assert_eq!(
            run_btormc(&bin, SAFE_BTOR2, DEFAULT_KMAX, DEFAULT_TIMEOUT).unwrap(),
            McVerdict::Safe
        );
    }

    #[test]
    #[ignore = "requires btormc (MUNUNU_BTORMC_PATH or $PATH); run with --ignored in mununu-sva"]
    fn decide_via_btormc_emits_and_decides() {
        // The full L2-seam → subprocess path: parse → emit_btor2 → btormc. A
        // reachable-bad model must come back Violated; a safe one Safe. This is
        // the emit hand-off end-to-end (the emitted text is btormc's input).
        let reach = crate::adapter::btor2::parser::parse(REACH_BTOR2).expect("parse reach");
        assert_eq!(
            decide_via_btormc(&reach, DEFAULT_KMAX, DEFAULT_TIMEOUT, None).unwrap(),
            McVerdict::Violated,
            "emit→btormc must decide the reachable-bad model as Violated"
        );
        let safe = crate::adapter::btor2::parser::parse(SAFE_BTOR2).expect("parse safe");
        assert_eq!(
            decide_via_btormc(&safe, DEFAULT_KMAX, DEFAULT_TIMEOUT, None).unwrap(),
            McVerdict::Safe,
            "emit→btormc must decide the safe model as Safe"
        );
    }
}
