//! R-S2b (Phase 9 §9.1) sub-item R-S2b.1 (2026-06-10) — Verilator
//! subprocess wrapper skeleton + binary discovery + version probe.
//!
//! # What this module does (R-S2b.1 scope)
//!
//! Locates a usable `verilator` binary and probes its version. Mirrors
//! the [`crate::adapter::cvc5`] subprocess-wrapper conventions
//! established for CVC5 — same `XxxBin` handle struct, same env-var
//! override (`MUNUNU_VERILATOR_PATH`), same structured `AdapterError`
//! on missing binary, same `#[ignore]`-gated integration test for the
//! present-binary path.
//!
//! Verilator is **optional at runtime**. R-S2b's parent purpose is to
//! seed predicate sets from observed steady-state register valuations
//! after a short concrete reset simulation (§Phase 9 R-S2b strategy).
//! When Verilator is absent, the caller is expected to log an
//! `AdapterWarning` and fall back to other Phase 9 strategies (R-S5
//! type-driven, R-S7 property-syntactic, etc.) — exactly the same
//! "optional subprocess, structured fallback" discipline R.5 Item 3
//! sub-item 3.4 established for CVC5 / Craig interpolation.
//!
//! # What this module does NOT do yet
//!
//! R-S2b is a multi-session arc; this sub-item ships ONLY the
//! discovery layer. Subsequent sub-items will add:
//!
//! - R-S2b.2 — Verilator compile invocation: given an SV file + top +
//!   reset signal name, produce a Verilator-compiled simulation
//!   binary in a temp dir. Mirror of `run_sv2v` / `run_yosys`.
//! - R-S2b.3 — reset-simulation harness: C++ testbench that drives
//!   the reset for K cycles + dumps register valuations; compile +
//!   run; capture steady-state.
//! - R-S2b.4 — steady-state → predicate-set integration. New
//!   `PredicateSource::ResetSimulation` variant feeding the existing
//!   seeding pipeline.
//! - R-S2b.5 — sidecar `simulate_reset: Option<SimulateReset>` field.
//! - R-S2b.6 — CLI flag + end-to-end integration test.
//!
//! # Why `MUNUNU_VERILATOR_PATH`
//!
//! Same rationale as `MUNUNU_CVC5_PATH` (CVC5 wrapper §"Pattern B
//! deployment"): Verilator is contributor-installed, not bundled in
//! `Dockerfile.dev`. Common install paths:
//!
//! - macOS Homebrew: `brew install verilator` → `/usr/local/bin/verilator`
//!   (Intel) or `/opt/homebrew/bin/verilator` (Apple Silicon).
//! - Debian / Ubuntu: `apt install verilator` → `/usr/bin/verilator`.
//! - Build-from-source: typically `/usr/local/bin/verilator`.

pub mod replay;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::adapter::{AdapterError, AdapterErrorKind};

/// Resolved Verilator binary + parsed version. Returned by
/// [`locate_verilator`].
#[derive(Debug, Clone)]
pub struct VerilatorBin {
    /// Resolved path to the Verilator binary. May be a bare name
    /// (`verilator`) if discovered on `$PATH`, or an absolute path
    /// if `MUNUNU_VERILATOR_PATH` is set.
    pub path: PathBuf,
    /// Parsed version string (e.g. `5.018`). `"<unparseable>"` when
    /// the `verilator --version` output doesn't match the expected
    /// format — discovery still succeeds; the version is
    /// diagnostic-only and does not gate functionality.
    pub version: String,
}

/// R-S2b.1 (2026-06-10) — locate a usable Verilator binary + probe
/// its version. Returns `Ok(VerilatorBin)` on success.
///
/// Returns `Err(AdapterError { kind: UnsupportedConstruct, ... })`
/// when the binary is absent or the version probe fails — callers
/// are expected to fall back gracefully (see R-S2b.4 + R-S2b.6 for
/// the predicate-seeding-pipeline wiring; the pattern is the same
/// optional-subprocess fallback CVC5 / Craig interpolation uses).
pub fn locate_verilator() -> Result<VerilatorBin, AdapterError> {
    // AR-GO-2 — shared locate body; see `crate::adapter::locate_tool`.
    let (path, version) = crate::adapter::locate_tool(
        "MUNUNU_VERILATOR_PATH",
        "verilator",
        "verilator",
        "Set MUNUNU_VERILATOR_PATH or install verilator ≥ 4.0 (Homebrew: \
         `brew install verilator`; Debian: `apt install verilator`).",
        parse_verilator_version,
    )?;
    Ok(VerilatorBin { path, version })
}

/// R-S2b.1 (2026-06-10) — extract the version token from
/// `verilator --version` output.
///
/// Verilator's `--version` output format (observed across 4.x and
/// 5.x releases):
/// ```text
/// Verilator 5.018 2024-01-07 rev (Homebrew v5.018)
/// ```
/// or, for older / from-source builds:
/// ```text
/// Verilator 4.228 2022-07-04 rev v4.228
/// ```
///
/// Returns `Some("5.018")` / `Some("4.228")` on success or `None`
/// when the format doesn't match — the caller treats this as
/// `"<unparseable>"` rather than a hard failure (the binary may
/// still be usable for actual queries; the version string is
/// diagnostic-only).
pub fn parse_verilator_version(output: &str) -> Option<String> {
    let first_line = output.lines().next()?;
    let trimmed = first_line.trim();
    let after_prefix = trimmed.strip_prefix("Verilator ")?;
    let end = after_prefix
        .find(char::is_whitespace)
        .unwrap_or(after_prefix.len());
    Some(after_prefix[..end].to_string())
}

// ─────────────────────────────────────────────────────────────────────
// R-S2b.2 (2026-06-10) — Verilator compile invocation
// ─────────────────────────────────────────────────────────────────────

/// Caller-facing options for a Verilator compile invocation.
///
/// Mirrors [`crate::adapter::yosys::YosysOptions`] in spirit — an
/// inert options bag the caller fills before invoking
/// [`compile_verilator`].
#[derive(Debug, Clone, Default)]
pub struct VerilatorOptions {
    /// Optional `--top-module <NAME>` override. When `None`,
    /// Verilator infers the top from the source file. The
    /// reset-simulation harness (R-S2b.3) normally passes
    /// `Some(top_name)` for determinism.
    pub top: Option<String>,
    /// Pass `-O3` to Verilator-generated C++. Defaults to `false`
    /// (use `-O0` for faster compile during the short
    /// reset-simulation runs). R-S2b's reset simulation is bounded
    /// to ~100 cycles per call, so simulation throughput is not the
    /// bottleneck.
    pub optimize: bool,
    /// Pass `-Wno-fatal` to demote warnings to non-fatal. Useful
    /// when the input SV emits widening / sign-mismatch warnings
    /// the user has triaged. Defaults to `false`.
    pub silence_warnings: bool,
    /// Pass `--public-flat-rw` to expose every internal signal as
    /// public on the generated C++ model — required so the
    /// reset-simulation testbench (R-S2b.3) can read internal
    /// register valuations without per-signal `/* verilator
    /// public */` annotations in the SV source. Defaults to
    /// `false` (only top-level ports are exposed; matches
    /// Verilator's default).
    pub expose_internal_signals: bool,
}

/// R-S2b.2 (2026-06-10) — pure helper that constructs the
/// `verilator` command-line argument vector for a compile
/// invocation, given an options bag + SV source + testbench C++
/// path + Verilator output directory (`--Mdir`).
///
/// **Pure**: does not invoke any subprocess. Testable in isolation
/// without a Verilator binary present.
///
/// **Output shape** (illustrative — order matters because the
/// non-flag positional arguments — the testbench `.cpp` and the
/// SV source — must come last):
/// ```text
/// --cc --exe --Mdir <mdir> [--top-module <top>] [-O3]
///   [-Wno-fatal] [--public-flat-rw] <tb_cpp> <sv_path>
/// ```
///
/// The caller passes the result to `std::process::Command::new(verilator).args(...)`.
pub fn build_verilator_compile_args(
    opts: &VerilatorOptions,
    sv_path: &Path,
    tb_cpp_path: &Path,
    mdir: &Path,
) -> Vec<OsString> {
    // `--cc` emits the C++ model; `--exe` tells Verilator to also
    // build an executable (using the supplied testbench .cpp +
    // generated `Mdir`). `--Mdir <mdir>` directs the per-call
    // work directory.
    let mut args: Vec<OsString> = vec![
        OsString::from("--cc"),
        OsString::from("--exe"),
        OsString::from("--Mdir"),
        OsString::from(mdir),
    ];
    // Optional `--top-module <NAME>`.
    if let Some(top) = &opts.top {
        args.push(OsString::from("--top-module"));
        args.push(OsString::from(top));
    }
    // Optional `-O3` (C++ optimisation level).
    if opts.optimize {
        args.push(OsString::from("-O3"));
    }
    // Optional `-Wno-fatal`.
    if opts.silence_warnings {
        args.push(OsString::from("-Wno-fatal"));
    }
    // Optional `--public-flat-rw` (expose internal signals to the C++
    // model so the reset-simulation testbench can read them).
    if opts.expose_internal_signals {
        args.push(OsString::from("--public-flat-rw"));
    }
    // Positional: testbench C++ first, then the SV source. Verilator
    // accepts these in any order, but a stable ordering eases
    // debugging.
    args.push(OsString::from(tb_cpp_path));
    args.push(OsString::from(sv_path));
    args
}

/// R-S2b.2 (2026-06-10) — derive the top-module name Verilator
/// will use for the generated header (`V<top>.h`) and Makefile
/// (`V<top>.mk`).
///
/// When the caller supplied `opts.top`, use it. Otherwise, fall
/// back to the SV file stem (Verilator's default behaviour when
/// `--top-module` is omitted is to use the filename without
/// extension as the top).
pub fn derive_top_name(opts: &VerilatorOptions, sv_path: &Path) -> String {
    if let Some(top) = &opts.top {
        return top.clone();
    }
    sv_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("top")
        .to_string()
}

/// R-S2b.2 (2026-06-10) — end-to-end Verilator compile + link.
///
/// 1. Writes `tb_cpp_content` to `<workdir>/tb.cpp`.
/// 2. Invokes `verilator --cc --exe ... tb.cpp sv_path` with the
///    `--Mdir <workdir>/obj_dir` argument so the generated C++
///    + Makefile land in a deterministic, per-call subdirectory.
/// 3. Invokes `make -C <obj_dir> -f V<top>.mk V<top>` to compile
///    + link the simulation binary.
/// 4. Returns the absolute path to the produced binary
///    (`<obj_dir>/V<top>`).
///
/// **Workdir lifetime is the caller's responsibility.** R-S2b.3
/// owns the [`VerilatorTempDir`] handle and drops it after
/// extracting the steady-state register valuation.
///
/// **Error handling**: failures spawn / non-zero exit at either
/// step return `AdapterError { kind: ParseError, ... }` (matching
/// the convention `run_yosys` + `run_sv2v` use). The Verilator
/// stderr is embedded in the error message so the user can
/// diagnose SV-parse failures + missing-construct gaps.
pub fn compile_verilator(
    verilator: &Path,
    opts: &VerilatorOptions,
    sv_path: &Path,
    tb_cpp_content: &str,
    workdir: &Path,
) -> Result<PathBuf, AdapterError> {
    let tb_cpp_path = workdir.join("tb.cpp");
    std::fs::write(&tb_cpp_path, tb_cpp_content).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/verilator: failed to write testbench to {}: {e}",
            tb_cpp_path.display()
        ),
        location: None,
    })?;

    let mdir = workdir.join("obj_dir");
    let args = build_verilator_compile_args(opts, sv_path, &tb_cpp_path, &mdir);

    let mut verilator_cmd = Command::new(verilator);
    verilator_cmd.args(&args);
    run_and_check(
        verilator_cmd,
        &format!("`{}`", verilator.display()),
        &format!("verilator for {}", sv_path.display()),
    )?;

    let top = derive_top_name(opts, sv_path);
    let mk_name = format!("V{top}.mk");
    let bin_name = format!("V{top}");
    let mut make_cmd = Command::new("make");
    make_cmd
        .arg("-C")
        .arg(&mdir)
        .arg("-f")
        .arg(&mk_name)
        .arg(&bin_name);
    run_and_check(
        make_cmd,
        &format!("`make` for {}", mdir.display()),
        &format!("make -f {mk_name} {bin_name} in {}", mdir.display()),
    )?;

    Ok(mdir.join(bin_name))
}

/// Q3 (§Phase 11 slot-3 close follow-up, 2026-06-12) — shared
/// "spawn + check exit status" subprocess helper for the
/// verilator-adapter call chain. Mirrors the
/// `crate::adapter::yosys::run_yosys` pattern (yosys/mod.rs:607)
/// which Q3's quality-session candidate cited as the precedent.
///
/// **Inputs**:
/// - `cmd`: a pre-configured `Command` (caller has already set
///   the binary + args).
/// - `bin_label`: short identifier for the spawn-error message
///   (typically the binary path in backticks). The full message
///   becomes `"adapter/verilator: failed to spawn <bin_label>: <io_err>"`.
/// - `context`: short description of the invocation for the
///   non-success error message (e.g. `"verilator for foo.sv"`,
///   `"make -f Vtop.mk Vtop in obj_dir"`).
///
/// **Behaviour**:
/// 1. Spawn via `cmd.output()`. On `Err`, return a
///    `ParseError`-kind `AdapterError` naming the bin_label +
///    the I/O error.
/// 2. If the child exited non-success, return a `ParseError`-kind
///    `AdapterError` embedding the exit status + stdout + stderr
///    (so users can diagnose Verilator parse failures + missing-
///    construct gaps inline).
/// 3. Otherwise return `Ok(())`.
fn run_and_check(mut cmd: Command, bin_label: &str, context: &str) -> Result<(), AdapterError> {
    let out = cmd.output().map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("adapter/verilator: failed to spawn {bin_label}: {e}"),
        location: None,
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/verilator: {context} exited with status {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                out.status,
            ),
            location: None,
        });
    }
    Ok(())
}

/// R-S2b.2 (2026-06-10) — per-call work directory for Verilator
/// compile + simulation. Mirrors `yosys::TempDir` (private to that
/// module per the "avoid pulling in another dep" convention).
///
/// Set `MUNUNU_KEEP_VERILATOR_TMP=1` to preserve the directory
/// after drop (useful when diagnosing a Verilator compile failure).
pub struct VerilatorTempDir {
    path: PathBuf,
    keep: bool,
}

impl VerilatorTempDir {
    /// Create a new per-call work directory under `$TMPDIR`. The
    /// directory name embeds pid + thread id + sequence + nanos so
    /// parallel tests in the same process do not collide.
    pub fn new() -> Result<Self, AdapterError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let tid = format!("{:?}", std::thread::current().id());
        let tid_digits: String = tid.chars().filter(|c| c.is_ascii_digit()).collect();
        let mut base = std::env::temp_dir();
        base.push(format!("mununu-verilator-{pid}-{tid_digits}-{seq}-{nanos}"));
        std::fs::create_dir_all(&base).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/verilator: failed to create tempdir at {}: {e}",
                base.display()
            ),
            location: None,
        })?;
        let keep = std::env::var("MUNUNU_KEEP_VERILATOR_TMP")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Ok(VerilatorTempDir { path: base, keep })
    }

    /// Path to the work directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for VerilatorTempDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// R-S2b.3a (2026-06-11) — reset-simulation data types + tb.cpp generator
// ─────────────────────────────────────────────────────────────────────
//
// R-S2b.3 is split across two sessions per the roadmap single-
// session policy:
//
//   R-S2b.3a (this commit) — data types + pure tb.cpp generator.
//     Unit-testable in isolation; no Verilator binary required.
//
//   R-S2b.3b (next session) — `run_reset_simulation` runner that
//     compiles the generated tb.cpp via R-S2b.2's
//     `compile_verilator`, runs the binary, and parses the dumped
//     register valuations. #[ignore]-gated integration test.

/// A single register's observed valuation after the configured
/// reset sequence. Produced by `run_reset_simulation` (R-S2b.3b).
///
/// The `value` field carries the bit-pattern interpretation
/// Verilator's `--public-flat-rw` exposes — a `uint32_t` /
/// `uint64_t` aliased view of the underlying SV wire / reg.
/// Widths > 64 are clipped (the dump format prints `0xff..ff`
/// when overflow occurs) — a follow-up may extend to multi-word
/// dumps if a fixture demands it. For the M.4 Caliptra fixture +
/// the OpenTitan-scale fixtures M.0–M.2 already validate, every
/// register that R-S2b is asked to observe fits in 64 bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterValuation {
    /// Register name as declared in the SV source (matches the
    /// Verilator-exposed signal handle).
    pub name: String,
    /// Observed value after the reset sequence (asserted for
    /// `hold_cycles`, then deasserted for `settle_cycles`).
    pub value: u64,
}

/// Configuration for a single reset-simulation run.
///
/// Carries the four facts the testbench generator needs to render
/// a deterministic reset sequence:
///
/// - which signal is the clock (toggled every cycle);
/// - which signal is the reset (held at `reset_asserted` during
///   reset, then held at `!reset_asserted` for the settle phase);
/// - how many cycles to hold reset asserted (`hold_cycles`) and
///   how many additional cycles to let the design settle before
///   sampling (`settle_cycles`);
/// - which register names to dump after the settle phase
///   (`observe_registers`).
///
/// Designed to be 1:1 with the sidecar `simulate_reset` field
/// R-S2b.5 will add. The same struct travels from sidecar →
/// generator → testbench source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetSimConfig {
    /// Top module name (matches `VerilatorOptions::top`). Drives
    /// the `V<top>` C++ class name in the generated testbench.
    pub top: String,
    /// SV signal name for the clock. The testbench toggles it
    /// `0 → 1 → 0` once per cycle.
    pub clock_signal: String,
    /// SV signal name for the reset. Held at `reset_asserted`
    /// while reset is active.
    pub reset_signal: String,
    /// Logical value to drive on `reset_signal` during the
    /// `hold_cycles` window. Use `1` for active-high resets, `0`
    /// for active-low.
    pub reset_asserted: u8,
    /// Cycles to hold the reset active. Caliptra's `soc_ifc_boot_fsm`
    /// fixture needs 1 cycle; OpenTitan `uart_tx` needs ~4.
    /// R-Y6 (§Phase 8) already supports K cycles for the
    /// bit-blaster's reset-sequence-aware init — this field mirrors
    /// it for the Verilator path.
    pub hold_cycles: u32,
    /// Cycles to run after deasserting reset, before sampling
    /// register valuations. Lets combinational outputs settle.
    /// Default `1` is sufficient for the M.0–M.4 fixtures.
    pub settle_cycles: u32,
    /// Register names (matching the SV declarations) to dump
    /// after the settle phase. Order is preserved in the output
    /// `Vec<RegisterValuation>`.
    pub observe_registers: Vec<String>,
}

impl ResetSimConfig {
    /// Validate a config before it reaches the testbench
    /// generator. Returns a structured `AdapterError` on:
    /// - empty `top`, `clock_signal`, or `reset_signal`;
    /// - `reset_asserted` outside `{0, 1}` (one-bit logical value);
    /// - empty `observe_registers` (a reset simulation with no
    ///   observed register is a no-op; the caller almost certainly
    ///   meant to populate this).
    ///
    /// Pure helper — no I/O.
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.top.trim().is_empty() {
            return Err(adapter_error("ResetSimConfig.top must be non-empty"));
        }
        if self.clock_signal.trim().is_empty() {
            return Err(adapter_error(
                "ResetSimConfig.clock_signal must be non-empty",
            ));
        }
        if self.reset_signal.trim().is_empty() {
            return Err(adapter_error(
                "ResetSimConfig.reset_signal must be non-empty",
            ));
        }
        if self.reset_asserted > 1 {
            return Err(adapter_error(&format!(
                "ResetSimConfig.reset_asserted must be 0 or 1; got {}",
                self.reset_asserted
            )));
        }
        if self.observe_registers.is_empty() {
            return Err(adapter_error(
                "ResetSimConfig.observe_registers must be non-empty \
                 (a reset-only simulation with nothing to observe is a no-op)",
            ));
        }
        Ok(())
    }
}

fn adapter_error(msg: &str) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!("adapter/verilator: {msg}"),
        location: None,
    }
}

/// R-S2b.3a (2026-06-11) — render the C++ testbench source for a
/// reset-simulation run, given a validated [`ResetSimConfig`].
///
/// **Pure**: no I/O, no subprocess invocation. Testable in
/// isolation via byte-equal comparison (the dump format is
/// stable).
///
/// # Generated testbench shape
///
/// The output is a single-`main` C++ source that:
///
/// 1. Includes `V<top>.h` and `verilated.h`.
/// 2. Instantiates `V<top> dut;`.
/// 3. Sets `dut.<reset_signal> = <reset_asserted>` and toggles
///    `dut.<clock_signal>` for `hold_cycles` complete cycles
///    (rising-edge advance + falling-edge advance per cycle).
/// 4. Flips reset to the deasserted value, then toggles the clock
///    for `settle_cycles` cycles.
/// 5. Dumps the observed registers to **stdout** in a stable,
///    line-delimited `name=0x<hex>` format that R-S2b.3b's parser
///    will consume. Example:
///    ```text
///    boot_fsm_ns=0x00000000
///    wait_count=0x00000003
///    ```
///
/// # Why `name=0x<hex>` on stdout
///
/// The simplest format that survives multiple toolchain
/// transitions (Verilator → C++ → make → process). No JSON
/// dependency in the testbench; no file-handle plumbing through
/// Verilator's exit path. The parser in R-S2b.3b is a one-pass
/// line scan.
///
/// # Dump-format invariants (load-bearing for R-S2b.3b)
///
/// - One register per line.
/// - `<name>=<value>` exactly (no whitespace before `=`, no
///   whitespace after `=`, no trailing whitespace).
/// - `<value>` is a `0x`-prefixed 16-char zero-padded hex literal
///   (64-bit width). R-S2b.3b's parser uses `0x` as a
///   sentinel for the value field.
/// - Lines NOT starting with one of the observed register names
///   are ignored (Verilator + C++ runtime may emit unrelated
///   stderr / stdout noise; the parser tolerates it).
///
/// Returns the testbench source as `String`. Caller writes it to
/// `<workdir>/tb.cpp` (R-S2b.2's `compile_verilator` already does
/// this writing step internally).
pub fn build_reset_simulation_tb_cpp(config: &ResetSimConfig) -> Result<String, AdapterError> {
    config.validate()?;

    let top = &config.top;
    let clk = &config.clock_signal;
    let rst = &config.reset_signal;
    let asserted = config.reset_asserted;
    let deasserted: u8 = 1 - asserted;
    let hold = config.hold_cycles;
    let settle = config.settle_cycles;

    let mut s = String::new();
    s.push_str("// R-S2b.3a — auto-generated reset-simulation testbench.\n");
    s.push_str("// DO NOT EDIT — regenerate via build_reset_simulation_tb_cpp.\n");
    s.push_str("#include \"verilated.h\"\n");
    s.push_str(&format!("#include \"V{top}.h\"\n"));
    s.push_str("#include <cstdio>\n");
    s.push_str("#include <cstdint>\n");
    s.push('\n');
    s.push_str("int main(int argc, char** argv) {\n");
    s.push_str("    Verilated::commandArgs(argc, argv);\n");
    s.push_str(&format!("    V{top}* dut = new V{top}();\n"));
    s.push_str("    // Phase 1 — assert reset for hold_cycles full cycles.\n");
    s.push_str(&format!("    dut->{rst} = {asserted};\n"));
    s.push_str(&format!("    for (uint32_t i = 0; i < {hold}; ++i) {{\n"));
    s.push_str(&format!("        dut->{clk} = 0;\n"));
    s.push_str("        dut->eval();\n");
    s.push_str(&format!("        dut->{clk} = 1;\n"));
    s.push_str("        dut->eval();\n");
    s.push_str("    }\n");
    s.push_str("    // Phase 2 — deassert reset and run settle_cycles.\n");
    s.push_str(&format!("    dut->{rst} = {deasserted};\n"));
    s.push_str(&format!("    for (uint32_t i = 0; i < {settle}; ++i) {{\n"));
    s.push_str(&format!("        dut->{clk} = 0;\n"));
    s.push_str("        dut->eval();\n");
    s.push_str(&format!("        dut->{clk} = 1;\n"));
    s.push_str("        dut->eval();\n");
    s.push_str("    }\n");
    s.push_str("    // Phase 3 — dump observed register valuations.\n");
    for reg in &config.observe_registers {
        s.push_str(&format!(
            "    printf(\"{reg}=0x%016llx\\n\", (unsigned long long)dut->{reg});\n"
        ));
    }
    s.push_str("    delete dut;\n");
    s.push_str("    return 0;\n");
    s.push_str("}\n");
    Ok(s)
}

/// P3 — configuration for a COUNTEREXAMPLE-REPLAY simulation: reset, then drive a per-cycle
/// input trace (the exact engine's witness), sampling the observed register(s) each cycle so a
/// caller can confirm the design reaches (and stays in) the predicted trap state at RTL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceReplayConfig {
    /// Top module name (drives the `V<top>` C++ class).
    pub top: String,
    /// Clock signal (toggled `0 → 1 → 0` once per cycle).
    pub clock_signal: String,
    /// Reset signal, held at `reset_asserted` for `hold_cycles`, then released.
    pub reset_signal: String,
    /// Logical reset-active value (`1` active-high, `0` active-low).
    pub reset_asserted: u8,
    /// Cycles to hold reset active before the drive phase.
    pub hold_cycles: u32,
    /// Inputs HELD at a fixed value across the whole run — the non-cone / idle-safe ports the
    /// witness does not constrain (driven once, before the trace).
    pub held_inputs: Vec<(String, u64)>,
    /// The per-cycle input assignments to drive (the counterexample's `inputs` sequence). Ports
    /// absent from a cycle keep their previous value.
    pub trace: Vec<Vec<(String, u64)>>,
    /// Register(s) sampled + printed AFTER each drive cycle, as `<name>=0x<hex>` (parsed by
    /// [`parse_reset_simulation_dump`]). The last cycle's sample is the trap check.
    pub observe_registers: Vec<String>,
}

/// P3 — render an input-driven counterexample-replay testbench: reset, drive [`TraceReplayConfig::trace`]
/// one input-set per cycle, and dump the observed registers after every cycle (so both the entry
/// into and the persistence in the trap state are visible). Pure — no I/O; feed the output to
/// [`compile_verilator`] and parse each cycle's dump with [`parse_reset_simulation_dump`].
pub fn build_trace_replay_tb_cpp(config: &TraceReplayConfig) -> Result<String, AdapterError> {
    if config.top.trim().is_empty() {
        return Err(adapter_error("TraceReplayConfig.top must be non-empty"));
    }
    if config.observe_registers.is_empty() {
        return Err(adapter_error(
            "TraceReplayConfig.observe_registers must name at least one register",
        ));
    }
    let top = &config.top;
    let clk = &config.clock_signal;
    let rst = &config.reset_signal;
    let asserted = config.reset_asserted;
    let deasserted: u8 = 1 - asserted;
    let hold = config.hold_cycles;

    let mut s = String::new();
    s.push_str("// P3 — auto-generated counterexample-replay testbench.\n");
    s.push_str("// DO NOT EDIT — regenerate via build_trace_replay_tb_cpp.\n");
    s.push_str("#include \"verilated.h\"\n");
    s.push_str(&format!("#include \"V{top}.h\"\n"));
    s.push_str("#include <cstdio>\n#include <cstdint>\n\n");
    s.push_str("static V");
    s.push_str(top);
    s.push_str("* dut;\n");
    s.push_str(&format!(
        "static void tick() {{ dut->{clk} = 0; dut->eval(); dut->{clk} = 1; dut->eval(); }}\n\n"
    ));
    s.push_str("int main(int argc, char** argv) {\n");
    s.push_str("    Verilated::commandArgs(argc, argv);\n");
    s.push_str(&format!("    dut = new V{top}();\n"));
    // Held (non-cone) inputs — driven once.
    for (name, val) in &config.held_inputs {
        s.push_str(&format!("    dut->{name} = {val}ull;\n"));
    }
    // Reset phase.
    s.push_str(&format!("    dut->{rst} = {asserted};\n"));
    s.push_str(&format!(
        "    for (uint32_t i = 0; i < {hold}; ++i) tick();\n"
    ));
    s.push_str(&format!("    dut->{rst} = {deasserted};\n"));
    // Drive phase — one input-set per cycle, dump the observed registers after each.
    for (cyc, step) in config.trace.iter().enumerate() {
        for (name, val) in step {
            s.push_str(&format!("    dut->{name} = {val}ull;\n"));
        }
        s.push_str("    tick();\n");
        for reg in &config.observe_registers {
            s.push_str(&format!(
                "    printf(\"cyc{cyc}:{reg}=0x%016llx\\n\", (unsigned long long)dut->{reg});\n"
            ));
        }
    }
    s.push_str("    delete dut;\n    return 0;\n}\n");
    Ok(s)
}

/// R-S2b.3a (2026-06-11) — pure parser for the testbench dump
/// format produced by [`build_reset_simulation_tb_cpp`].
///
/// Scans each line of `stdout` for the `<name>=0x<hex>` pattern.
/// Lines that don't match the expected shape are silently
/// skipped (Verilator + C++ runtime may emit unrelated noise).
/// The output preserves the order in which the registers appeared
/// in the input.
///
/// Returns `Vec<RegisterValuation>`. Empty on no matches — the
/// caller (R-S2b.3b) is expected to cross-check the length
/// against `config.observe_registers.len()` and emit an
/// `AdapterWarning` on under-counting.
pub fn parse_reset_simulation_dump(stdout: &str) -> Vec<RegisterValuation> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        // Expected shape: `<name>=0x<hex>`.
        let Some(eq) = line.find('=') else {
            continue;
        };
        let (name, rest) = line.split_at(eq);
        // Strip the leading `=`.
        let value_str = &rest[1..];
        let Some(hex_str) = value_str
            .strip_prefix("0x")
            .or_else(|| value_str.strip_prefix("0X"))
        else {
            continue;
        };
        if name.is_empty() || hex_str.is_empty() {
            continue;
        }
        if let Ok(value) = u64::from_str_radix(hex_str, 16) {
            out.push(RegisterValuation {
                name: name.to_string(),
                value,
            });
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// R-S2b.3b (2026-06-11) — end-to-end reset-simulation runner
// ─────────────────────────────────────────────────────────────────────

/// R-S2b.3b (2026-06-11) — derive the [`VerilatorOptions`] the
/// reset-simulation harness should pass to [`compile_verilator`],
/// starting from the caller's base options.
///
/// The runner always forces two options regardless of the caller's
/// base:
///
/// - `top = Some(config.top.clone())` — `compile_verilator` uses
///   this to derive the `V<top>.h` header + `V<top>.mk` Makefile
///   name. The testbench source embeds `V<top>` literally, so the
///   compile + the testbench must agree.
/// - `expose_internal_signals = true` — required for the testbench
///   to read internal registers via `dut->reg_name`. Without
///   `--public-flat-rw`, Verilator hides everything but top-level
///   ports.
///
/// All other options (`optimize`, `silence_warnings`) carry
/// through from the caller's base unchanged.
///
/// **Pure**: no I/O. Unit-testable in isolation.
pub fn derive_simulation_options(
    base: &VerilatorOptions,
    config: &ResetSimConfig,
) -> VerilatorOptions {
    VerilatorOptions {
        top: Some(config.top.clone()),
        optimize: base.optimize,
        silence_warnings: base.silence_warnings,
        expose_internal_signals: true,
    }
}

/// R-S2b.3b (2026-06-11) — verify the parsed dump contains every
/// register the config asked to observe. Returns the names of
/// missing registers (preserving the declaration order in
/// `config.observe_registers`) — empty when complete.
///
/// **Pure**: no I/O. The caller decides whether a missing
/// register is an error or a warning. The strict R-S2b.3b
/// runner returns an error; R-S2b.4 (predicate-seeding pipeline)
/// can choose to keep going with whatever the dump captured.
pub fn missing_observed_registers(
    config: &ResetSimConfig,
    valuations: &[RegisterValuation],
) -> Vec<String> {
    let present: std::collections::HashSet<&str> =
        valuations.iter().map(|v| v.name.as_str()).collect();
    config
        .observe_registers
        .iter()
        .filter(|name| !present.contains(name.as_str()))
        .cloned()
        .collect()
}

/// R-S2b.3b (2026-06-11) — end-to-end reset-simulation runner.
///
/// 1. Validates the `config` (rejects empty signal names + bad
///    `reset_asserted` + empty `observe_registers`).
/// 2. Derives the effective `VerilatorOptions` (forces `top` +
///    `expose_internal_signals`).
/// 3. Renders the testbench C++ source from `config` via
///    [`build_reset_simulation_tb_cpp`].
/// 4. Compiles the design + testbench via [`compile_verilator`]
///    (R-S2b.2); returns the produced binary path.
/// 5. Spawns the binary, captures stdout.
/// 6. Parses stdout via [`parse_reset_simulation_dump`].
/// 7. Cross-checks that every `config.observe_registers` entry
///    appeared in the dump. If any are missing, returns an
///    `AdapterError` naming them (Verilator's `--public-flat-rw`
///    sometimes fails to expose a signal when the user mistyped
///    a register name — the error message points the user at
///    the right fix).
/// 8. Returns the valuations in the order declared in
///    `config.observe_registers`.
///
/// **Workdir lifetime** is the caller's responsibility. Typical
/// usage:
/// ```ignore
/// let tmp = VerilatorTempDir::new()?;
/// let valuations = run_reset_simulation(&bin.path, &opts, sv, &cfg, tmp.path())?;
/// // tmp drops + removes the workdir here (unless
/// // MUNUNU_KEEP_VERILATOR_TMP=1).
/// ```
pub fn run_reset_simulation(
    verilator: &Path,
    base_opts: &VerilatorOptions,
    sv_path: &Path,
    config: &ResetSimConfig,
    workdir: &Path,
) -> Result<Vec<RegisterValuation>, AdapterError> {
    config.validate()?;
    let opts = derive_simulation_options(base_opts, config);
    let tb_cpp_content = build_reset_simulation_tb_cpp(config)?;
    let bin_path = compile_verilator(verilator, &opts, sv_path, &tb_cpp_content, workdir)?;

    let out = Command::new(&bin_path).output().map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/verilator: failed to spawn reset-simulation binary {}: {e}",
            bin_path.display()
        ),
        location: None,
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/verilator: reset-simulation binary {} exited with status {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                bin_path.display(),
                out.status
            ),
            location: None,
        });
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let valuations = parse_reset_simulation_dump(&stdout);

    let missing = missing_observed_registers(config, &valuations);
    if !missing.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/verilator: reset-simulation dump missing registers: {}. \
                 Check the register names in ResetSimConfig.observe_registers \
                 against the SV declarations + confirm \
                 --public-flat-rw exposed them (current opts: expose_internal_signals=true).\nstdout:\n{stdout}",
                missing.join(", ")
            ),
            location: None,
        });
    }

    Ok(valuations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P3 — the counterexample-replay testbench builder renders a reset phase, the held-input
    /// setup, one input-drive-then-sample per trace cycle, and a per-cycle observed-register dump.
    #[test]
    fn build_trace_replay_tb_cpp_renders_reset_drive_and_sample() {
        let cfg = TraceReplayConfig {
            top: "fsm".to_string(),
            clock_signal: "clk_i".to_string(),
            reset_signal: "rst_ni".to_string(),
            reset_asserted: 0, // active-low
            hold_cycles: 3,
            held_inputs: vec![("enable_i".to_string(), 1)],
            trace: vec![
                vec![("esc_i".to_string(), 1)],
                vec![("esc_i".to_string(), 0)],
            ],
            observe_registers: vec!["state_q".to_string()],
        };
        let src = build_trace_replay_tb_cpp(&cfg).expect("render");
        // Reset: active-low held then released.
        assert!(src.contains("dut->rst_ni = 0;"), "asserts reset");
        assert!(src.contains("dut->rst_ni = 1;"), "releases reset");
        assert!(
            src.contains("for (uint32_t i = 0; i < 3; ++i) tick();"),
            "holds reset 3 cyc"
        );
        // Held input driven once.
        assert!(
            src.contains("dut->enable_i = 1ull;"),
            "drives the held input"
        );
        // Both trace cycles' inputs.
        assert!(
            src.matches("dut->esc_i = 1ull;").count() == 1
                && src.matches("dut->esc_i = 0ull;").count() == 1,
            "drives esc_i per trace cycle",
        );
        // A per-cycle sample of the observed register.
        assert!(
            src.contains("cyc0:state_q=0x"),
            "samples state_q on cycle 0"
        );
        assert!(
            src.contains("cyc1:state_q=0x"),
            "samples state_q on cycle 1"
        );
    }

    #[test]
    fn locate_verilator_returns_structured_error_when_binary_absent() {
        // Force a definitely-missing path via the env var. The
        // function MUST return a structured AdapterError (not panic)
        // so callers can fall back gracefully.
        // SAFETY: env vars are process-global; this test sets +
        // restores to avoid leaking the bogus path to other tests
        // that may discover via PATH.
        let original = std::env::var("MUNUNU_VERILATOR_PATH").ok();
        // SAFETY: required for env var manipulation in tests; the
        // value is restored before the test returns.
        unsafe {
            std::env::set_var(
                "MUNUNU_VERILATOR_PATH",
                "/nonexistent/path/to/verilator/binary/definitely/not/here",
            );
        }
        let result = locate_verilator();
        unsafe {
            match original {
                Some(v) => std::env::set_var("MUNUNU_VERILATOR_PATH", v),
                None => std::env::remove_var("MUNUNU_VERILATOR_PATH"),
            }
        }
        assert!(
            result.is_err(),
            "locate_verilator MUST return Err when the binary is absent; got {result:?}"
        );
        let err = result.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        assert!(
            err.message.contains("verilator"),
            "error message MUST mention verilator for diagnosability; got: {}",
            err.message
        );
    }

    #[test]
    #[ignore = "requires verilator binary installed; run with --ignored when available"]
    fn locate_verilator_succeeds_when_binary_available() {
        // R-S2b.1 integration test that's ignored by default. Run
        // with `cargo test -- --ignored` when Verilator is
        // installed locally (e.g. `brew install verilator` first).
        let result = locate_verilator();
        match result {
            Ok(bin) => {
                assert!(
                    bin.version != "<unparseable>",
                    "expected a parseable version from `verilator --version`; got {}",
                    bin.version
                );
            }
            Err(e) => panic!(
                "locate_verilator failed when Verilator should be installed (run \
                 `brew install verilator` or set MUNUNU_VERILATOR_PATH first): {}",
                e.message
            ),
        }
    }

    #[test]
    fn parse_verilator_version_matches_homebrew_5x_format() {
        let output = "Verilator 5.018 2024-01-07 rev (Homebrew v5.018)\n";
        assert_eq!(parse_verilator_version(output), Some("5.018".to_string()));
    }

    #[test]
    fn parse_verilator_version_matches_legacy_4x_format() {
        let output = "Verilator 4.228 2022-07-04 rev v4.228\n";
        assert_eq!(parse_verilator_version(output), Some("4.228".to_string()));
    }

    #[test]
    fn parse_verilator_version_returns_none_on_unknown_format() {
        let output = "some other tool that is not verilator\n";
        assert_eq!(parse_verilator_version(output), None);
    }

    #[test]
    fn parse_verilator_version_returns_none_on_empty_output() {
        assert_eq!(parse_verilator_version(""), None);
    }

    // ─────────────────────────────────────────────────────────────
    // R-S2b.2 tests — compile-invocation args + tempdir
    // ─────────────────────────────────────────────────────────────

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn build_args_includes_cc_and_exe() {
        let opts = VerilatorOptions::default();
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(args.contains(&os("--cc")), "args must include --cc");
        assert!(args.contains(&os("--exe")), "args must include --exe");
    }

    #[test]
    fn build_args_includes_mdir_flag_and_path() {
        let opts = VerilatorOptions::default();
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        // --Mdir followed by the actual directory.
        let i = args
            .iter()
            .position(|a| a == &os("--Mdir"))
            .expect("--Mdir must be present");
        assert_eq!(
            args.get(i + 1),
            Some(&os("/work/obj_dir")),
            "--Mdir must be followed by the obj_dir path"
        );
    }

    #[test]
    fn build_args_includes_top_module_when_set() {
        let opts = VerilatorOptions {
            top: Some("amba_arbiter".to_string()),
            ..VerilatorOptions::default()
        };
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        let i = args
            .iter()
            .position(|a| a == &os("--top-module"))
            .expect("--top-module must be present when opts.top is set");
        assert_eq!(args.get(i + 1), Some(&os("amba_arbiter")));
    }

    #[test]
    fn build_args_omits_top_module_when_none() {
        let opts = VerilatorOptions::default();
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(
            !args.contains(&os("--top-module")),
            "--top-module must be omitted when opts.top is None; got {args:?}"
        );
    }

    #[test]
    fn build_args_includes_optimize_when_set() {
        let opts = VerilatorOptions {
            optimize: true,
            ..VerilatorOptions::default()
        };
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(args.contains(&os("-O3")));
    }

    #[test]
    fn build_args_includes_wno_fatal_when_set() {
        let opts = VerilatorOptions {
            silence_warnings: true,
            ..VerilatorOptions::default()
        };
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(args.contains(&os("-Wno-fatal")));
    }

    #[test]
    fn build_args_omits_optimize_and_warnings_by_default() {
        let opts = VerilatorOptions::default();
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(!args.contains(&os("-O3")));
        assert!(!args.contains(&os("-Wno-fatal")));
    }

    #[test]
    fn build_args_places_tb_and_sv_at_end_in_order() {
        // Verilator accepts these in any order, but a stable
        // ordering eases reproduction + debugging. The contract is:
        // tb.cpp comes before the .sv source.
        let opts = VerilatorOptions::default();
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        let tb_i = args.iter().position(|a| a == &os("/work/tb.cpp"));
        let sv_i = args.iter().position(|a| a == &os("/work/design.sv"));
        assert!(tb_i.is_some() && sv_i.is_some());
        assert!(
            tb_i.unwrap() < sv_i.unwrap(),
            "tb.cpp must come before design.sv"
        );
    }

    #[test]
    fn derive_top_name_uses_opts_when_set() {
        let opts = VerilatorOptions {
            top: Some("explicit_top".to_string()),
            ..VerilatorOptions::default()
        };
        assert_eq!(
            derive_top_name(&opts, Path::new("/work/design.sv")),
            "explicit_top"
        );
    }

    #[test]
    fn derive_top_name_falls_back_to_file_stem() {
        let opts = VerilatorOptions::default();
        assert_eq!(
            derive_top_name(&opts, Path::new("/work/amba_arbiter.sv")),
            "amba_arbiter"
        );
    }

    #[test]
    fn verilator_tempdir_creates_unique_directories() {
        // Two tempdirs in the same process must not collide. The
        // pid + thread + seq + nanos suffix guarantees this.
        let d1 = VerilatorTempDir::new().expect("tempdir 1");
        let d2 = VerilatorTempDir::new().expect("tempdir 2");
        assert_ne!(d1.path(), d2.path());
        assert!(d1.path().exists());
        assert!(d2.path().exists());
    }

    #[test]
    fn verilator_tempdir_removes_directory_on_drop() {
        let path = {
            let d = VerilatorTempDir::new().expect("tempdir");
            assert!(d.path().exists());
            d.path().to_path_buf()
            // drop here
        };
        assert!(
            !path.exists(),
            "VerilatorTempDir must remove its directory on drop (unless MUNUNU_KEEP_VERILATOR_TMP=1)"
        );
    }

    #[test]
    #[ignore = "requires verilator + make installed; run with --ignored when available"]
    fn compile_verilator_produces_runnable_binary_on_trivial_module() {
        // End-to-end integration test: a minimal SV module + a
        // no-op testbench compile cleanly + produce a binary that
        // exits 0 when run. Gated because Verilator is contributor-
        // installed (matches the locate_verilator integration-test
        // gating).
        let bin = locate_verilator().expect("verilator must be installed for this test");
        let tmp = VerilatorTempDir::new().expect("tempdir");
        let sv_path = tmp.path().join("trivial.sv");
        std::fs::write(&sv_path, "module trivial(input wire clk); endmodule\n").expect("write sv");
        let tb =
            "#include \"Vtrivial.h\"\nint main(int,char**){ Vtrivial m; m.eval(); return 0; }\n";
        let opts = VerilatorOptions {
            top: Some("trivial".to_string()),
            ..VerilatorOptions::default()
        };
        let bin_path = compile_verilator(&bin.path, &opts, &sv_path, tb, tmp.path())
            .expect("compile_verilator must succeed on the trivial module");
        assert!(
            bin_path.exists(),
            "binary path {} must exist",
            bin_path.display()
        );
        let status = Command::new(&bin_path)
            .status()
            .expect("spawn compiled binary");
        assert!(
            status.success(),
            "compiled binary must exit 0; got {status}"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // R-S2b.3a tests — data types + tb.cpp generator + dump parser
    // ─────────────────────────────────────────────────────────────

    fn sample_config() -> ResetSimConfig {
        ResetSimConfig {
            top: "amba_arbiter".to_string(),
            clock_signal: "clk".to_string(),
            reset_signal: "rst_n".to_string(),
            reset_asserted: 0,
            hold_cycles: 4,
            settle_cycles: 1,
            observe_registers: vec!["burst_count".to_string(), "grant_state".to_string()],
        }
    }

    #[test]
    fn build_args_includes_public_flat_rw_when_set() {
        let opts = VerilatorOptions {
            expose_internal_signals: true,
            ..VerilatorOptions::default()
        };
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(args.contains(&os("--public-flat-rw")));
    }

    #[test]
    fn build_args_omits_public_flat_rw_by_default() {
        let opts = VerilatorOptions::default();
        let args = build_verilator_compile_args(
            &opts,
            Path::new("/work/design.sv"),
            Path::new("/work/tb.cpp"),
            Path::new("/work/obj_dir"),
        );
        assert!(!args.contains(&os("--public-flat-rw")));
    }

    #[test]
    fn validate_accepts_well_formed_config() {
        assert!(sample_config().validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_top() {
        let mut c = sample_config();
        c.top = "".to_string();
        let err = c.validate().unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        assert!(err.message.contains("top"));
    }

    #[test]
    fn validate_rejects_empty_clock() {
        let mut c = sample_config();
        c.clock_signal = "".to_string();
        let err = c.validate().unwrap_err();
        assert!(err.message.contains("clock_signal"));
    }

    #[test]
    fn validate_rejects_empty_reset() {
        let mut c = sample_config();
        c.reset_signal = "".to_string();
        let err = c.validate().unwrap_err();
        assert!(err.message.contains("reset_signal"));
    }

    #[test]
    fn validate_rejects_reset_asserted_above_1() {
        let mut c = sample_config();
        c.reset_asserted = 2;
        let err = c.validate().unwrap_err();
        assert!(err.message.contains("reset_asserted"));
    }

    #[test]
    fn validate_rejects_empty_observe_registers() {
        let mut c = sample_config();
        c.observe_registers.clear();
        let err = c.validate().unwrap_err();
        assert!(err.message.contains("observe_registers"));
    }

    #[test]
    fn tb_cpp_includes_dut_header_and_handle() {
        let src = build_reset_simulation_tb_cpp(&sample_config()).expect("ok");
        assert!(
            src.contains("#include \"Vamba_arbiter.h\""),
            "tb.cpp must include the generated V<top>.h header"
        );
        assert!(
            src.contains("Vamba_arbiter* dut = new Vamba_arbiter()"),
            "tb.cpp must instantiate the V<top> class via heap allocation \
             (Verilator's recommended pattern; survives lifetime + cleanup)"
        );
    }

    #[test]
    fn tb_cpp_holds_reset_for_configured_cycles() {
        let mut c = sample_config();
        c.hold_cycles = 7;
        c.settle_cycles = 2;
        let src = build_reset_simulation_tb_cpp(&c).expect("ok");
        // Loop bound for the hold phase.
        assert!(
            src.contains("i < 7"),
            "tb.cpp must encode hold_cycles=7 as the loop bound; got:\n{src}"
        );
        // Loop bound for the settle phase.
        assert!(
            src.contains("i < 2"),
            "tb.cpp must encode settle_cycles=2 as the loop bound; got:\n{src}"
        );
    }

    #[test]
    fn tb_cpp_asserts_active_low_reset_correctly() {
        let mut c = sample_config();
        c.reset_asserted = 0;
        let src = build_reset_simulation_tb_cpp(&c).expect("ok");
        // Phase 1: rst_n = 0; Phase 2: rst_n = 1.
        assert!(src.contains("dut->rst_n = 0;"));
        assert!(src.contains("dut->rst_n = 1;"));
        // The phase-1 assignment must come before the phase-2 one.
        let i_assert = src.find("dut->rst_n = 0;").unwrap();
        let i_deassert = src.find("dut->rst_n = 1;").unwrap();
        assert!(
            i_assert < i_deassert,
            "phase 1 (assert) must precede phase 2 (deassert)"
        );
    }

    #[test]
    fn tb_cpp_asserts_active_high_reset_correctly() {
        let mut c = sample_config();
        c.reset_asserted = 1;
        c.reset_signal = "rst".to_string();
        let src = build_reset_simulation_tb_cpp(&c).expect("ok");
        assert!(src.contains("dut->rst = 1;"));
        assert!(src.contains("dut->rst = 0;"));
        let i_assert = src.find("dut->rst = 1;").unwrap();
        let i_deassert = src.find("dut->rst = 0;").unwrap();
        assert!(i_assert < i_deassert);
    }

    #[test]
    fn tb_cpp_emits_printf_per_observed_register() {
        let src = build_reset_simulation_tb_cpp(&sample_config()).expect("ok");
        // One printf per register; the format string is the
        // dump-format invariant.
        assert!(src.contains("printf(\"burst_count=0x%016llx\\n\""));
        assert!(src.contains("printf(\"grant_state=0x%016llx\\n\""));
        // The signal-handle cast is present.
        assert!(src.contains("(unsigned long long)dut->burst_count"));
        assert!(src.contains("(unsigned long long)dut->grant_state"));
    }

    #[test]
    fn tb_cpp_preserves_register_order() {
        let c = ResetSimConfig {
            observe_registers: vec!["zzz".to_string(), "aaa".to_string()],
            ..sample_config()
        };
        let src = build_reset_simulation_tb_cpp(&c).expect("ok");
        let i_zzz = src.find("zzz=").unwrap();
        let i_aaa = src.find("aaa=").unwrap();
        assert!(
            i_zzz < i_aaa,
            "register declaration order must be preserved (zzz declared before aaa)"
        );
    }

    #[test]
    fn tb_cpp_rejects_invalid_config() {
        let mut c = sample_config();
        c.observe_registers.clear();
        assert!(build_reset_simulation_tb_cpp(&c).is_err());
    }

    // R-S2b.3a — dump-format parser tests.

    #[test]
    fn parse_dump_extracts_single_register() {
        let stdout = "boot_fsm_ns=0x0000000000000003\n";
        let out = parse_reset_simulation_dump(stdout);
        assert_eq!(
            out,
            vec![RegisterValuation {
                name: "boot_fsm_ns".to_string(),
                value: 0x3
            }]
        );
    }

    #[test]
    fn parse_dump_extracts_multiple_registers_in_order() {
        let stdout = "burst_count=0x000000000000000f\ngrant_state=0x0000000000000002\n";
        let out = parse_reset_simulation_dump(stdout);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "burst_count");
        assert_eq!(out[0].value, 0xf);
        assert_eq!(out[1].name, "grant_state");
        assert_eq!(out[1].value, 0x2);
    }

    #[test]
    fn parse_dump_skips_noise_lines() {
        let stdout = "Verilator startup banner\nboot_fsm_ns=0x0000000000000005\nrandom log line\n";
        let out = parse_reset_simulation_dump(stdout);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "boot_fsm_ns");
        assert_eq!(out[0].value, 0x5);
    }

    #[test]
    fn parse_dump_accepts_uppercase_0x_prefix() {
        let stdout = "x=0XFF\n";
        let out = parse_reset_simulation_dump(stdout);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, 0xff);
    }

    #[test]
    fn parse_dump_skips_lines_without_equals() {
        let stdout = "header line\n";
        assert_eq!(parse_reset_simulation_dump(stdout), vec![]);
    }

    #[test]
    fn parse_dump_skips_lines_without_0x_prefix() {
        let stdout = "reg=42\n"; // decimal, not the expected hex format
        assert_eq!(parse_reset_simulation_dump(stdout), vec![]);
    }

    #[test]
    fn parse_dump_skips_unparseable_hex() {
        let stdout = "reg=0xnothex\n";
        assert_eq!(parse_reset_simulation_dump(stdout), vec![]);
    }

    #[test]
    fn parse_dump_round_trips_via_generator() {
        // The generator's printf format ↔ parser regex must be in
        // lockstep. This test guards the invariant: the parser
        // must successfully extract exactly the registers the
        // generator declared, given a synthetic stdout that
        // matches the generator's printf format byte-for-byte.
        let config = sample_config();
        // Synthesise stdout the way the generated tb.cpp would
        // produce it for arbitrary hard-coded valuations.
        let synthetic_stdout = format!(
            "burst_count=0x{:016x}\ngrant_state=0x{:016x}\n",
            0xdeadbeefu64, 0x42u64,
        );
        let out = parse_reset_simulation_dump(&synthetic_stdout);
        assert_eq!(out.len(), config.observe_registers.len());
        assert_eq!(out[0].name, "burst_count");
        assert_eq!(out[0].value, 0xdeadbeef);
        assert_eq!(out[1].name, "grant_state");
        assert_eq!(out[1].value, 0x42);
    }

    // ─────────────────────────────────────────────────────────────
    // R-S2b.3b tests — derive_simulation_options + missing_observed_registers
    // + #[ignore]-gated end-to-end runner
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn derive_simulation_options_forces_top_and_public_flat_rw() {
        let base = VerilatorOptions {
            top: None,
            optimize: true,
            silence_warnings: true,
            expose_internal_signals: false, // even when false in base, runner must force true
        };
        let cfg = sample_config();
        let derived = derive_simulation_options(&base, &cfg);
        assert_eq!(derived.top, Some("amba_arbiter".to_string()));
        assert!(
            derived.expose_internal_signals,
            "runner must always set expose_internal_signals=true"
        );
        // Other options carry through from base.
        assert!(derived.optimize);
        assert!(derived.silence_warnings);
    }

    #[test]
    fn derive_simulation_options_overrides_base_top() {
        // Even when the caller's base specifies a different top, the
        // runner forces config.top so the testbench (which embeds
        // V<config.top> literally) stays consistent with the compile.
        let base = VerilatorOptions {
            top: Some("different_top".to_string()),
            ..VerilatorOptions::default()
        };
        let cfg = sample_config();
        let derived = derive_simulation_options(&base, &cfg);
        assert_eq!(derived.top, Some("amba_arbiter".to_string()));
    }

    #[test]
    fn missing_observed_registers_empty_on_complete_dump() {
        let cfg = sample_config();
        let valuations = vec![
            RegisterValuation {
                name: "burst_count".to_string(),
                value: 0,
            },
            RegisterValuation {
                name: "grant_state".to_string(),
                value: 0,
            },
        ];
        assert!(missing_observed_registers(&cfg, &valuations).is_empty());
    }

    #[test]
    fn missing_observed_registers_lists_missing_in_declaration_order() {
        let cfg = ResetSimConfig {
            observe_registers: vec!["zzz".to_string(), "aaa".to_string(), "mmm".to_string()],
            ..sample_config()
        };
        // Only `aaa` showed up.
        let valuations = vec![RegisterValuation {
            name: "aaa".to_string(),
            value: 0,
        }];
        let missing = missing_observed_registers(&cfg, &valuations);
        // Order preserved: zzz declared first, then mmm.
        assert_eq!(missing, vec!["zzz".to_string(), "mmm".to_string()]);
    }

    #[test]
    fn missing_observed_registers_ignores_extra_dumped_registers() {
        // The runner should NOT complain about extra registers in
        // the dump — Verilator may surface internal signals the
        // caller didn't ask for. Missing is the failure mode, not
        // surplus.
        let cfg = ResetSimConfig {
            observe_registers: vec!["a".to_string()],
            ..sample_config()
        };
        let valuations = vec![
            RegisterValuation {
                name: "a".to_string(),
                value: 0,
            },
            RegisterValuation {
                name: "b_extra".to_string(),
                value: 0,
            },
        ];
        assert!(missing_observed_registers(&cfg, &valuations).is_empty());
    }

    #[test]
    #[ignore = "requires verilator + make installed; run with --ignored when available"]
    fn run_reset_simulation_returns_post_reset_counter_value() {
        // End-to-end runner test on a small synchronous counter:
        // - reset is active-low, held for 1 cycle (counter → 0).
        // - settle 2 cycles deasserted (counter → 2).
        // - observe `count`; expect value 2.
        let bin = locate_verilator().expect("verilator must be installed for this test");
        let tmp = VerilatorTempDir::new().expect("tempdir");
        let sv_path = tmp.path().join("simple_counter.sv");
        std::fs::write(
            &sv_path,
            concat!(
                "module simple_counter (\n",
                "    input wire clk,\n",
                "    input wire rst_n,\n",
                "    output reg [3:0] count\n",
                ");\n",
                "    always @(posedge clk or negedge rst_n) begin\n",
                "        if (!rst_n) count <= 4'd0;\n",
                "        else count <= count + 4'd1;\n",
                "    end\n",
                "endmodule\n",
            ),
        )
        .expect("write sv");

        let cfg = ResetSimConfig {
            top: "simple_counter".to_string(),
            clock_signal: "clk".to_string(),
            reset_signal: "rst_n".to_string(),
            reset_asserted: 0,
            hold_cycles: 1,
            settle_cycles: 2,
            observe_registers: vec!["count".to_string()],
        };
        let base = VerilatorOptions::default();

        let valuations = run_reset_simulation(&bin.path, &base, &sv_path, &cfg, tmp.path())
            .expect("run_reset_simulation must succeed");
        assert_eq!(valuations.len(), 1);
        assert_eq!(valuations[0].name, "count");
        assert_eq!(
            valuations[0].value, 2,
            "after 1-cycle reset + 2 settle cycles, the counter must read 2; got {valuations:?}"
        );
    }
}
