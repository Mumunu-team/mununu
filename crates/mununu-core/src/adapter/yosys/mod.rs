//! Yosys child-process driver — elaborates SystemVerilog to BTOR2.
//!
//! # Architecture (per §13.4 of the RTL roadmap)
//!
//! Yosys is invoked **as a child process** via [`std::process::Command`].
//! No linking. No FFI. Yosys is ISC-licensed; the integration is license-clean
//! for any mununu downstream license.
//!
//! ```text
//!   .sv source ─► tempdir/work.sv
//!                       │
//!                       ▼  yosys -p "read_verilog -formal -sv ...; prep -top X; chformal -lower; write_btor ..."
//!                       │
//!                  tempdir/design.btor
//!                       │
//!                       ▼  super::btor2::parser::parse(...)
//!                  Btor2File ─► AdapterIR ─► CTXDSL
//! ```
//!
//! # Hard rules
//!
//! - **Never enable Yosys's optional Verific frontend.** It is commercial and
//!   would taint the binary. The driver passes only `-p` (no `-Q -F` or
//!   plugin-load arguments) and verifies (best-effort) that the binary in
//!   `$PATH` is a stock build by checking the `-V` banner for `verific`.
//! - **No persistent state in the working tree.** The driver writes only to
//!   a per-call tempdir which is removed (or kept for inspection if
//!   `MUNUNU_KEEP_YOSYS_TMP=1` is set).
//!
//! # Phase 1 scope
//!
//! - Single SV file or multiple SV files passed as `additional_sources`.
//! - User specifies the top module via [`YosysOptions::top`] or it is inferred
//!   from the file name (best-effort).
//! - Multi-clock SVA, classes, and Verific-only constructs are out of scope —
//!   Yosys errors will surface with a layer-tagged context (`adapter/yosys: ...`).

/// R-MM (KMTS multi-module composition) — netlist-driven driver.
pub mod multi_module;
pub mod source_manifest;

use crate::adapter::{
    AdapterError, AdapterErrorKind, AdapterOptions, AdapterOutput, FormatAdapter, SourceFormat,
    SourceInfo,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Per-invocation wall-clock cap for the per-module BTOR2 emission path
/// ([`translate_sv_per_module`]). That path runs one FULL yosys elaboration PER
/// submodule (re-reading the whole source set each time) and is designed for the
/// SMALL multi-module fixtures the M.0–M.4 milestones use, where each call is
/// sub-second. On a large real design a single call can thrash for minutes (or
/// exhaust memory); this bound turns that into a clean, explained failure instead
/// of an indefinite hang. Generous (60×+ the expected per-call time) so it never
/// trips on the fixtures the feature actually targets.
const PER_MODULE_YOSYS_TIMEOUT: Duration = Duration::from_secs(60);

/// RTL front-end selection for the SV → BTOR2 lift.
///
/// - `Auto` (default) — the historical behaviour: yosys `read_verilog` (with
///   optional sv2v), unless `MUNUNU_YOSYS_FRONTEND=slang` is set in the env and
///   the plugin is present, in which case slang is used.
/// - `Verilog` — force yosys `read_verilog` (+ sv2v per `use_sv2v`), ignoring the
///   env override.
/// - `Slang` — force the yosys-slang plugin (`read_slang`), a full SystemVerilog
///   front-end that accepts constructs `read_verilog` and sv2v reject (bounded
///   `while` loops, `module M import pkg::*;`, …). Errors clearly if the plugin is
///   not found. Validated in the `mununu-sva` image (the plugin ships there).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SvFrontend {
    /// Env-driven (default): `MUNUNU_YOSYS_FRONTEND=slang` → slang, else read_verilog.
    #[default]
    Auto,
    /// Force yosys `read_verilog` (+ sv2v per `use_sv2v`).
    Verilog,
    /// Force the slang front-end (`read_slang`); error if the plugin is absent.
    Slang,
}

impl SvFrontend {
    /// Human-readable lift label for diagnostics (the `lift-frontend` note, #466).
    /// `Auto` is a request, not an outcome — a resolved lift is always `Verilog`/`Slang`.
    pub fn lift_label(self) -> &'static str {
        match self {
            SvFrontend::Slang => "yosys-slang (read_slang)",
            SvFrontend::Verilog => "read_verilog + sv2v",
            SvFrontend::Auto => "auto",
        }
    }
}

/// Yosys-specific options.
#[derive(Debug, Clone, Default)]
pub struct YosysOptions {
    /// Top-level module (if empty, the driver lets Yosys auto-detect).
    pub top: Option<String>,
    /// Additional SV source files to compile alongside the primary input.
    pub additional_sources: Vec<(String, String)>,
    /// Extra on-disk include-search directories, forwarded to the RTL
    /// front-end as `-I<dir>` flags (yosys `read_verilog` / `read_slang`,
    /// and the sv2v preprocessor). This lets a caller resolve
    /// `` `include "frag.vh" `` directives against the *original* source
    /// tree WITHOUT having to pass every include fragment as a compiled
    /// `additional_sources` entry — passing a mid-module fragment as a
    /// standalone source makes the parser compile it in isolation, which
    /// fails (`syntax error, unexpected TOK_ELSE/TOK_ENDMODULE`). The
    /// per-design source-manifest lift (multi-file AssertLLM2 corpus)
    /// populates this so only real compilation units are read while
    /// includes still resolve.
    ///
    /// **Default**: empty (only the staging tempdir + primary source dir
    /// are searched, the historical behaviour). Populated from the CLI
    /// `--include-dir` flag; no API/UI analog (those pass source *content*
    /// by name, so an on-disk search directory is a local-filesystem-only
    /// concept — the flat name-staging of `additional_sources` already
    /// resolves cross-file includes on those surfaces).
    pub extra_include_dirs: Vec<PathBuf>,
    /// Module-parameter overrides applied BEFORE elaboration sizes the design —
    /// yosys `chparam -set <name> <value> <module>` (before `hierarchy`) on the
    /// read_verilog frontend, or slang `-G <name>=<value>` (at read time, since
    /// slang elaborates then) on the slang frontend. Each entry is `(lhs, value)`
    /// where `lhs` is a bare parameter name
    /// `NAME` (applied to the top module) or a scoped `MODULE.NAME` (applied to
    /// that module). This lets a caller shrink a parameterised timing interval so
    /// the design's counters get smaller — a 20000-cycle power-up wait sizing a
    /// 15-bit counter becomes a 10-bit counter at `INIT_WAIT=4` — without a wrapper
    /// module (which would rename the SVA atoms and break binding).
    ///
    /// SOUNDNESS / scope: the verdict is scoped to the overridden parameters
    /// (echoed in the transcript + JSON report). A parameter yosys cannot apply
    /// (not a parameter of the target module) is an ERROR surfaced from the lift —
    /// never a silent drop (contrast `--config-value`, mununu#459).
    ///
    /// **Default**: empty. Populated from the CLI `--param NAME=VALUE` flag / the
    /// API `params` field. No `read_slang` analog is needed — `chparam` operates
    /// on the elaborated netlist regardless of front-end.
    pub params: Vec<(String, String)>,
    /// Skip the Verific-taint check (for testing).
    pub skip_verific_check: bool,
    /// Optional original path of the primary SV source. Used only to
    /// rewrite the `source_file` field of any auto-emitted black-box
    /// sidecar (Document B task B3) — yosys sees the SV through a
    /// per-call tempdir, so without this hint the sidecars would point
    /// at `/tmp/.../work.sv`. The user expects the path of the file
    /// they actually invoked mununu on.
    pub primary_source_path: Option<String>,
    /// Run `sv2v` as a preprocessing pass before Yosys. Unblocks Yosys's
    /// built-in parser for modern SystemVerilog constructs it doesn't
    /// accept — most notably the SV2009/2012 module-header import
    /// syntax `module M import pkg::*; (ports);` used by Caliptra-RTL,
    /// OpenTitan, ibex, cv32e40p, and similar open-source RTL.
    ///
    /// Requires `sv2v` (zachjs/sv2v ≥ 0.0.10 recommended) on `$PATH` or
    /// in `MUNUNU_SV2V_PATH`. Defaults to `false` so existing examples
    /// keep their current parse behavior. Opt in via the CLI flag
    /// `--preprocessor sv2v` or the API field `use_sv2v: true` on the
    /// import request.
    pub use_sv2v: bool,
    /// RTL front-end selection. `Auto` (default) preserves the env-driven
    /// behaviour; `Slang` forces the yosys-slang plugin (`read_slang`), which
    /// lifts modern-SV constructs `read_verilog`/sv2v reject (`while` loops,
    /// `module M import pkg::*;`). Populated from the CLI `--frontend` flag / the
    /// API `use_slang` field. See [`SvFrontend`].
    pub frontend: SvFrontend,
    /// When `true`, Yosys's `setundef -anyseq` pass replaces every
    /// undefined net with a fresh symbolic choice instead of pinning
    /// it to zero. Preserves CWE-1245-class bug-bearing semantics
    /// (the unmatched-case path admits any value) at the cost of
    /// introducing `$anyseq` state cells — the bit-blast state-bit
    /// count can grow substantially. **Default: `false`** (matches
    /// historical `setundef -zero` behaviour; small state space; bug
    /// silently transformed away). See the SOUNDNESS comment in
    /// [`build_script`] for the trade-off.
    ///
    /// **Precedence**: when both `setundef_anyseq` and
    /// [`Self::setundef_anyconst`] are `true`, `setundef_anyseq`
    /// wins (it is the strictly more permissive policy).
    pub setundef_anyseq: bool,
    /// R-Y1 (§Phase 8) — When `true`, Yosys's `setundef -anyconst`
    /// pass replaces every undefined net with a single nondeterministic
    /// constant input (NOT a per-cycle state cell). The solver chooses
    /// any concrete value at init; the value is then fixed for the
    /// entire run. Strictly between `-zero` (deterministic; masks
    /// bugs) and `-anyseq` (per-cycle havoc; state-space explosion):
    /// adds `|undef_bits|` constant *inputs* without inflating the
    /// per-cycle state count.
    ///
    /// **The intermediate the Caliptra CWE-1245 fixture has been
    /// waiting for.** Under `-zero` the bug encodings `{5, 6, 7}` of
    /// `boot_fsm_ns` are unreachable from the deterministic 0-init;
    /// under `-anyseq` they appear but the state-bit count exceeds
    /// `MAX_STATE_BITS = 20`. Under `-anyconst` the solver picks an
    /// init in `{0..7}` and the run proceeds with that init held
    /// constant; the bug-bearing encodings become reachable without
    /// per-cycle havoc inflation.
    ///
    /// **Default: `false`** (preserves the historical `-zero`
    /// behaviour; existing fixtures' verdicts unchanged). Opt in
    /// via the API field or the CLI flag once R.0a / R.0b expose
    /// it.
    ///
    /// **Precedence**: when both this and `setundef_anyseq` are
    /// `true`, `setundef_anyseq` wins (strictly more permissive).
    pub setundef_anyconst: bool,
    /// R-Y2 (§Phase 8 §8.1) — Per-signal init-policy overrides:
    /// `(signal_name, InitPolicy)` pairs. The Yosys script-builder
    /// emits `setattr -set <attr> <val> w:<signal>` for each entry
    /// between `hierarchy` and `proc`, giving surgical control over
    /// individual signals. **Per-signal overrides take precedence
    /// over the global `setundef_*` flags for the signals listed**
    /// — e.g. set `setundef_anyconst = false` globally but list
    /// `("boot_fsm_ns", InitPolicy::Anyconst)` in this vector to
    /// apply anyconst only to that one signal (the Caliptra fixture
    /// pattern; closes §Phase 8 §8.2's load-bearing fix).
    ///
    /// **Default**: empty vec (no overrides; global policy applies
    /// to all signals). Populated by `crates/mununu-cli/src/loader.rs`
    /// from the sidecar's `SvAnnotation::init_policy_overrides()` —
    /// see that helper for the deterministic ordering.
    pub init_policy_overrides: InitPolicyOverrides,
    /// When `true`, the per-submodule BTOR2 emission path is engaged
    /// (R.0b — KMTS pivot). Each submodule reachable from the top is
    /// emitted as its own BTOR2 file by running Yosys once per
    /// submodule with `hierarchy -top <m>` (no `flatten`). The R.2
    /// KMTS lifter consumes the per-submodule outputs; the top-level
    /// netlist drives composition (per
    /// [`docs/design/native-sv-abstraction.md`](../../docs/design/native-sv-abstraction.md)
    /// §3 and §4).
    ///
    /// This is *independent* of the legacy single-BTOR2 path: callers
    /// that set this opt out of the existing `translate_sv` and use
    /// [`translate_sv_per_module`] instead.
    pub per_module_btor: bool,
    /// When set, the per-submodule BTOR2 files are persisted to this
    /// directory (one `<module_name>.btor2` per submodule) for
    /// downstream consumption / inspection. The directory is created
    /// if missing. When `None` (default), the per-submodule files live
    /// in a transient per-call tempdir and are dropped after the
    /// per-module translation completes — the [`AdapterOutput`]s still
    /// carry the CLTS, sidecars, and partition summaries the BTOR2
    /// adapter produced.
    pub per_module_output_dir: Option<PathBuf>,
    /// Control-slice cut points: net names to replace with a free
    /// `$anyseq` input in the SV → BTOR2 lift (Yosys `cutpoint w:<net>`),
    /// spliced after `proc`/`write_json` and before `flatten`. The net's
    /// datapath fanin then drops out via cone-of-influence — the sound,
    /// netlist-level way to shrink a wide FSM's cone so `--engine
    /// exact-symbolic` fits (the `control_slice.py` prototype done in the
    /// engine; here `cutpoint` handles reg/wire/width/multi-driver
    /// uniformly, no source rewriting).
    ///
    /// **SOUNDNESS — this is an OVER-APPROXIMATION.** A freed net becomes
    /// nondeterministic, which only *adds* transitions. A definite HOLDS
    /// therefore transfers to the concrete RTL (safety + over-approx =
    /// sound).
    ///
    /// That claim was **false in practice until mununu#498**: `cutpoint`
    /// emits the freed net as a Yosys `$anyseq`, which BTOR2 encodes as a
    /// `state` with no `next` line, and the engines read such a state as a
    /// *held register* pinned to its (absent, therefore zero) init. The
    /// freed net was nailed to 0 — an UNDER-approximation, the exact
    /// opposite of what this paragraph promises, so a cut could turn a
    /// concretely-false safety property into `HOLDS`. A `state` with no
    /// `next` is now classified as a free input at the STS-IR seam
    /// (`BtorSts::states_with_next`), and
    /// `e2e_cutpoint_stays_an_over_approximation_no_verdict_flips` pins the
    /// guarantee end-to-end: no property may change verdict when a cut is
    /// applied to the reduced `mini_fetch` FSM.
    ///
    /// A definite VIOLATED is sound only when the counterexample
    /// is guard-independent — the canonical case being an *orphaned* FSM
    /// state (in-degree 0), which no freed guard can make reachable, so
    /// `AG EF <orphaned-state>` stays soundly VIOLATED. A general VIOLATED
    /// under cut points may be spurious; verify-auto surfaces a
    /// `control-slice` ScopeCaveat note when this vector is non-empty.
    ///
    /// Names are validated to a safe identifier charset before being
    /// interpolated into the Yosys `-p` script (no injection). Default:
    /// empty (no cut points).
    pub cutpoint_signals: Vec<String>,
}

/// SV-via-Yosys adapter — wraps the BTOR2 path with a Yosys subprocess.
pub struct YosysAdapter;

impl FormatAdapter for YosysAdapter {
    fn detect(_content: &str) -> bool {
        // Detection of "this is SV that should go through Yosys" happens at
        // the format-routing layer (CLI flag `--frontend yosys`, or the
        // `.sv`/`.v` extension when the user opts into the Yosys path).
        // The bare-content detector returns false so auto_translate does
        // not accidentally route plain SV here.
        false
    }

    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
        translate_sv(content, options, &YosysOptions::default())
    }
}

/// Translate SystemVerilog source to AdapterOutput by invoking Yosys and
/// reading back the BTOR2.
/// The flattened-design artifacts produced by [`run_sv_flatten_btor2`]:
/// the BTOR2 text, the pre-flatten hierarchy JSON (empty when Yosys did
/// not emit it), and the staged primary-source path (for rewriting
/// tempdir paths back to the user's invocation path).
struct SvFlattenArtifacts {
    btor2: String,
    hier_json: String,
    staged_primary: String,
    /// The concrete front end that produced this BTOR2 (`Verilog`/`Slang`, never
    /// `Auto`) — surfaced by verify-auto's `lift-frontend` note (#466).
    frontend_used: SvFrontend,
    /// When `Auto` fell back (an earlier front end errored before this one
    /// succeeded), the prior attempt's error — else `None`. (#466)
    fallback_reason: Option<String>,
}

/// Run sv2v (optional) + the flattened Yosys script and return the
/// design's BTOR2 text plus the hierarchy snapshot. Shared by
/// [`translate_sv`] (which bit-blasts the BTOR2) and
/// [`sv_discover_state_cells`] (which only reads the BTOR2's named state
/// cells), so the sv2v include-path + Yosys-invocation logic lives in one
/// place.
/// Net-type keywords that yosys `read_verilog` / the slang lowering reject in a
/// PORT header (`input tri0 baud8x`). A net type on a *port* only governs that
/// port's EXTERNAL resolution (a shared bus with pulls / wired-logic across
/// modules); verifying the module in isolation, an input is havoc'd and an
/// output is driven by the module's own logic, so dropping the qualifier is
/// SOUND — and for inputs it is *exact* (mununu havocs every input regardless of
/// net type). Without this the whole design is rejected at parse.
const PORT_NET_TYPE_KEYWORDS: &[&str] = &[
    "tri0", "tri1", "triand", "trior", "trireg", "wand", "wor", "supply0", "supply1", "uwire",
    "tri",
];

/// Drop a rejected net-type qualifier that directly follows a port direction
/// keyword (`input`/`output`/`inout`) — an A2 lift-widening normalization.
///
/// SCOPE: **ports only.** INTERNAL net-type declarations are left untouched:
/// their pull (`tri0` reads 0 undriven) and wired-resolution (`wand`/`wor`)
/// semantics DO affect the modeled behaviour, and stripping them soundly depends
/// on the `setundef` polarity (default `-zero` matches `tri0` but not `tri1`) —
/// a separate, gated follow-up. Port stripping needs no such gate because a port
/// net type is invisible to isolated single-module verification.
///
/// Tokenizes past `//`/`/* */` comments and `"…"` strings so a `tri0` inside a
/// comment/string is never rewritten, and skips escaped identifiers (`\tri0`).
/// A no-op (clone) when there is no such qualifier — safe to run on every staged
/// source. `input`/`output`/`inout` are reserved keywords (never identifiers),
/// so a net-type word immediately after one is unambiguously a port qualifier.
pub(crate) fn strip_port_net_types(src: &str) -> String {
    const DIRECTIONS: &[&str] = &["input", "output", "inout"];
    let bytes = src.as_bytes();
    // (start, end) byte spans of identifier/keyword words that lie in CODE
    // (outside comments/strings, not escaped identifiers).
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else if c == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            // A leading backslash makes it an escaped identifier (a NAME) — skip.
            if start == 0 || bytes[start - 1] != b'\\' {
                words.push((start, i));
            }
        } else {
            i += 1;
        }
    }
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for w in words.windows(2) {
        let d = &src[w[0].0..w[0].1];
        let n = &src[w[1].0..w[1].1];
        if DIRECTIONS.contains(&d) && PORT_NET_TYPE_KEYWORDS.contains(&n) {
            removals.push((w[1].0, w[1].1));
        }
    }
    if removals.is_empty() {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len());
    let mut last = 0;
    for (s, e) in removals {
        out.push_str(&src[last..s]);
        // Swallow the whitespace after the removed keyword so `input tri0 x`
        // collapses to `input x`, not `input  x`.
        let mut e2 = e;
        while e2 < bytes.len() && (bytes[e2] == b' ' || bytes[e2] == b'\t') {
            e2 += 1;
        }
        last = e2;
    }
    out.push_str(&src[last..]);
    out
}

/// The SV → flattened-BTOR2 lift (verification-execution-planner P1.1). When the frontend is
/// [`SvFrontend::Auto`] it tries the `read_verilog`(+sv2v) path first and, on a hard lift ERROR,
/// **falls back to slang** — which parses SystemVerilog natively (module-header package imports
/// `read_verilog` rejects with `unexpected TOK_IMPORT`, `while` loops, `import pkg::*`). This
/// unlocks the OpenTitan-class designs the default frontend cannot lift while keeping the broad
/// default for everything else. Fallback fires only on `Err` (never a vacuous success, so the
/// sv2v-drops-SVA trap can't be masked); an EXPLICIT `Verilog`/`Slang` request is honoured as-is
/// with no fallback. Preference is env-driven (`MUNUNU_YOSYS_FRONTEND=slang` tries slang first).
fn run_sv_flatten_btor2(
    content: &str,
    yopts: &YosysOptions,
) -> Result<SvFlattenArtifacts, AdapterError> {
    if yopts.frontend != SvFrontend::Auto {
        return run_sv_flatten_btor2_concrete(content, yopts);
    }
    let slang_ok = locate_slang_plugin().is_some();
    let prefer_slang = std::env::var("MUNUNU_YOSYS_FRONTEND")
        .map(|v| v.eq_ignore_ascii_case("slang"))
        .unwrap_or(false);
    // Track the prior failed attempt so a successful FALLBACK can name what it fell
    // back from + why (#466) — a silent front-end swap is the #459 failure class.
    let mut prior: Option<(SvFrontend, AdapterError)> = None;
    for fe in frontend_attempt_order(slang_ok, prefer_slang) {
        let mut y = yopts.clone();
        y.frontend = fe;
        match run_sv_flatten_btor2_concrete(content, &y) {
            Ok(mut a) => {
                if let Some((prior_fe, e)) = prior {
                    a.fallback_reason =
                        Some(format!("{} failed: {}", prior_fe.lift_label(), e.message));
                }
                return Ok(a);
            }
            Err(e) => prior = Some((fe, e)),
        }
    }
    Err(prior
        .map(|(_, e)| e)
        .expect("Auto frontend has at least the Verilog attempt"))
}

/// The ordered concrete frontends an [`SvFrontend::Auto`] lift attempts (P1.1 fallback). The
/// broad `read_verilog`(+sv2v) path leads (no plugin needed, handles inout / use-before-decl that
/// yosys-slang rejects); slang is the fallback (module-header package imports, `while` loops).
/// `MUNUNU_YOSYS_FRONTEND=slang` flips the preference. slang is included only when its plugin is
/// present — otherwise the sole attempt is `read_verilog` (unchanged legacy behaviour).
fn frontend_attempt_order(slang_available: bool, prefer_slang: bool) -> Vec<SvFrontend> {
    match (slang_available, prefer_slang) {
        (true, true) => vec![SvFrontend::Slang, SvFrontend::Verilog],
        (true, false) => vec![SvFrontend::Verilog, SvFrontend::Slang],
        (false, _) => vec![SvFrontend::Verilog],
    }
}

fn run_sv_flatten_btor2_concrete(
    content: &str,
    yopts: &YosysOptions,
) -> Result<SvFlattenArtifacts, AdapterError> {
    let yosys = locate_yosys()?;
    if !yopts.skip_verific_check {
        verify_no_verific(&yosys)?;
    }

    let tmp = TempDir::new("mununu-yosys")?;
    let primary = tmp.path().join("work.sv");
    // A2 — strip yosys/slang-rejected net-type qualifiers on port headers
    // (`input tri0 x`) so the design parses; sound (a port net type is invisible
    // to isolated single-module verification). No-op on sources without them.
    write_file(&primary, &strip_port_net_types(content))?;

    let mut sources = vec![primary.clone()];
    for (name, src) in &yopts.additional_sources {
        let p = tmp.path().join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        write_file(&p, &strip_port_net_types(src))?;
        sources.push(p);
    }

    // Optional sv2v preprocessing pass. sv2v translates modern SV
    // constructs Yosys's built-in parser doesn't accept (notably the
    // module-header `import pkg::*;` syntax) into Verilog-2005 that
    // Yosys handles cleanly. Activated by `YosysOptions::use_sv2v`
    // (the only switch — wire `--preprocessor sv2v` on the CLI or
    // `use_sv2v: true` on the API into this field).
    //
    // A3 — the opt-in slang RTL frontend (`MUNUNU_YOSYS_FRONTEND=slang`). `read_slang`
    // parses SystemVerilog natively (a superset of what `read_verilog` accepts), so sv2v
    // is redundant with it and is skipped when slang is active.
    // Front-end selection: an explicit `Slang` request forces the plugin (and
    // errors if it is absent — never a silent fall-back to read_verilog, which
    // would defeat the whole point of asking for slang); `Verilog` forces
    // read_verilog; `Auto` keeps the env-driven default.
    let slang_plugin = match yopts.frontend {
        SvFrontend::Slang => Some(locate_slang_plugin().ok_or_else(|| {
            AdapterError {
                kind: AdapterErrorKind::UnsupportedConstruct,
                message: "adapter/yosys: the slang RTL front-end was requested (--frontend slang \
                      / use_slang) but the yosys-slang plugin was not found. Set \
                      MUNUNU_YOSYS_SLANG_PLUGIN to slang.so, or run in the mununu-sva image."
                    .into(),
                location: None,
            }
        })?),
        SvFrontend::Verilog => None,
        SvFrontend::Auto => slang_frontend_selection(),
    };
    // The concrete front end this lift resolved to (#466). `Auto` is never seen here
    // for the fallback loop (it sets a concrete `fe` per attempt); directly-called
    // `Auto` resolves via the plugin presence above.
    let frontend_used = if slang_plugin.is_some() {
        SvFrontend::Slang
    } else {
        SvFrontend::Verilog
    };
    if yopts.use_sv2v && slang_plugin.is_none() {
        let sv2v = locate_sv2v()?;
        let preprocessed = tmp.path().join("preprocessed.sv");
        // Include-search path. Two sources, in priority order:
        //
        // 1. The staging tempdir itself. Every `additional_sources` entry
        //    is written here (see above), so an `\`include "X.sv"` whose
        //    target is one of the staged sources — e.g. real OpenTitan RTL
        //    doing `\`include "prim_assert.sv"` where the stub prim_assert
        //    is passed as an additional source (the verify-path /
        //    multi-file case, which never sets `primary_source_path`) —
        //    resolves against the staged copy. sv2v does not search the
        //    including file's own directory by default, so without this the
        //    include fails even though the file is right next to it.
        //
        // 2. The parent dir of the primary source on disk, if known.
        //    `caliptra_sva.svh` and similar header files live next to the
        //    `.sv` the user invoked us on (the `context eval` single-file
        //    case); sv2v can't see them otherwise because mununu staged a
        //    per-call tempdir. Canonicalized to an absolute path so
        //    relative invocations (e.g. `mununu context eval foo.sv` from
        //    the source dir) resolve to a usable `-I`.
        let mut include_dirs: Vec<PathBuf> = vec![tmp.path().to_path_buf()];
        include_dirs.extend(yopts.primary_source_path.as_ref().and_then(|p| {
            let abs = Path::new(p)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(p));
            abs.parent().map(Path::to_path_buf)
        }));
        // 3. Caller-supplied include dirs (the source-manifest lift path).
        include_dirs.extend(yopts.extra_include_dirs.iter().cloned());
        run_sv2v(&sv2v, &sources, &include_dirs, &preprocessed)?;
        // Replace the original .sv inputs with the single combined
        // Verilog-2005 file. sv2v resolves cross-file packages,
        // interfaces, and parameters in one pass, so feeding the
        // combined output to Yosys is the documented correct usage.
        sources = vec![preprocessed];
    }

    let btor_path = tmp.path().join("design.btor");
    let hier_json_path = tmp.path().join("hier.json");
    let script = build_script(
        &sources,
        yopts.top.as_deref(),
        &btor_path,
        &hier_json_path,
        yopts.setundef_anyseq,
        yopts.setundef_anyconst,
        &yopts.init_policy_overrides,
        &yopts.cutpoint_signals,
        slang_plugin.is_some(),
        &yopts.extra_include_dirs,
        &yopts.params,
    );

    let mut yosys_cmd = Command::new(&yosys);
    if let Some(plugin) = &slang_plugin {
        yosys_cmd.arg("-m").arg(plugin);
    }
    yosys_cmd.arg("-q").arg("-p").arg(&script);
    // mununu#504 — bounded. This ran as a bare `.output()`, i.e. forever. The lift precedes every
    // property, so a hung yosys emits ZERO output and the whole run reads as absent — one of the
    // two ways a consumer loses a full gate run. `run_yosys` 700 lines below has used this same
    // helper for the per-module path all along.
    let outcome = match crate::adapter::lift_timeout() {
        Some(t) => crate::adapter::run_with_timeout(&mut yosys_cmd, None, t),
        None => yosys_cmd.output().map(|o| {
            Some((
                o.status,
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ))
        }),
    }
    .map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("adapter/yosys: failed to spawn yosys: {e}"),
        location: None,
    })?;
    let (status, stdout_s, stderr_s) = lift_timeout_outcome(outcome, "yosys")?;
    let output = std::process::Output {
        status,
        stdout: stdout_s.into_bytes(),
        stderr: stderr_s.into_bytes(),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: yosys exited with status {} \nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            ),
            location: None,
        });
    }

    let btor2 = std::fs::read_to_string(&btor_path).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: yosys ran but did not produce {} ({e})",
            btor_path.display()
        ),
        location: None,
    })?;

    // Best-effort hierarchy snapshot (port directions + blackbox scan);
    // empty when Yosys did not emit it.
    let hier_json = std::fs::read_to_string(&hier_json_path).unwrap_or_default();

    Ok(SvFlattenArtifacts {
        btor2,
        hier_json,
        staged_primary: primary.to_string_lossy().into_owned(),
        frontend_used,
        fallback_reason: None,
    })
}

/// A named state cell discovered in a flattened SV design — the
/// post-`flatten` dotted-instance name (`u_chan0.prediv_q`) the
/// bit-blaster sees, plus its register width. Surfaced by
/// [`sv_discover_state_cells`] so authors can populate a sidecar's
/// `signals[]` without a manual Yosys netlist dump (sidecar-audit C1.1 /
/// finding E1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvStateCell {
    /// The cell's Yosys symbol (e.g. `u_chan0.prediv_q`).
    pub name: String,
    /// The register's bit width.
    pub width: u32,
}

/// Drive sv2v → Yosys-flatten → BTOR2 and return the design's named state
/// cells (sidecar-audit C1.1 / finding E1). Unlike [`translate_sv`], this
/// does NOT bit-blast, so it succeeds on cap-busting designs — exactly the
/// case where the author needs the dotted state-cell names to write a
/// param-concretization sidecar. Cells without a Yosys symbol (synthetic
/// `chformal`-lowered flops) are skipped. Sorted by name; deduped.
pub fn sv_discover_state_cells(
    content: &str,
    yopts: &YosysOptions,
) -> Result<Vec<SvStateCell>, AdapterError> {
    use super::btor2::ast::Node;
    let artifacts = run_sv_flatten_btor2(content, yopts)?;
    let file = super::btor2::parser::parse(&artifacts.btor2).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("adapter/yosys: discover could not parse the BTOR2: {e}"),
        location: None,
    })?;
    let symbols = super::btor2::parser::collect_symbols(&file);
    let mut cells: Vec<SvStateCell> = file
        .lines
        .iter()
        .filter_map(|line| match &line.node {
            Node::State { sort, .. } => {
                let name = symbols.get(&line.nid)?.clone();
                let width = super::btor2::parser::bv_width(&file, *sort)?;
                Some(SvStateCell { name, width })
            }
            _ => None,
        })
        .collect();
    cells.sort_by(|a, b| a.name.cmp(&b.name));
    cells.dedup();
    Ok(cells)
}

pub fn translate_sv(
    content: &str,
    options: &AdapterOptions,
    yopts: &YosysOptions,
) -> Result<AdapterOutput, AdapterError> {
    let SvFlattenArtifacts {
        btor2: btor,
        hier_json,
        staged_primary,
        ..
    } = run_sv_flatten_btor2(content, yopts)?;

    // Document B task B1: capture the top module's port directions
    // from the pre-flatten hierarchy snapshot, feed them through the
    // BTOR2 reader so it can classify inputs by direction instead of
    // defaulting every input to `Uncontrollable`. Falls back to the
    // historical behaviour if the hierarchy file isn't readable.
    let top_directions: std::collections::HashMap<
        String,
        crate::controllability::BoundaryDirection,
    > = if hier_json.is_empty() {
        Default::default()
    } else {
        parse_top_module_port_directions(&hier_json, yopts.top.as_deref())
    };

    let btor_options = if top_directions.is_empty() {
        options.clone()
    } else {
        crate::adapter::AdapterOptions {
            port_directions: top_directions,
            ..options.clone()
        }
    };

    // Hand the BTOR2 to the BTOR2 adapter.
    let mut out =
        super::btor2::Btor2Adapter::translate(&btor, &btor_options).map_err(|mut e| {
            e.message = format!("adapter/yosys: BTOR2 reader failed: {}", e.message);
            e
        })?;

    // Re-tag the source format so users see this as the SV-via-Yosys path.
    out.source_info = SourceInfo {
        format: SourceFormat::SystemVerilog,
        ..out.source_info
    };

    // Document B task B2 + B3 (yosys half): scan the pre-flatten
    // hierarchy snapshot for `(* blackbox *)` modules and auto-emit
    // `BlackBoxInterface.json` + `GapMarkerReport.json` sidecars for
    // each. The hierarchy file is best-effort — if yosys did not
    // produce it, we proceed without sidecars rather than failing the
    // whole translation.
    if !hier_json.is_empty() {
        let mut blackboxes = parse_blackbox_modules(&hier_json);
        // Rewrite the tempdir path back to the user's primary source
        // path when known. Yosys saw the SV as `<tempdir>/work.sv`;
        // the user expects to see the path they actually invoked
        // mununu on.
        if let Some(real_path) = yopts.primary_source_path.as_ref() {
            for bb in blackboxes.iter_mut() {
                if let Some(ref src) = bb.source_file
                    && src == &staged_primary
                {
                    bb.source_file = Some(real_path.clone());
                }
            }
        }
        if !blackboxes.is_empty() {
            let opts = crate::contract::discover::DiscoverOptions {
                force_controllable: &[],
                force_uncontrollable: &[],
                emit_fairness_gap: false,
                corpus: None,
            };
            let sidecars = crate::contract::discover::build_blackbox_sidecars(&blackboxes, &opts);
            out.sidecars.extend(sidecars);
        }
    }

    Ok(out)
}

/// Run sv2v (optional) + the flattened Yosys script and return *just*
/// the design's BTOR2 text. This is the SV-direct CEGAR one-call
/// frontend: `mununu sv cegar` and `POST /api/v1/sv/cegar` lift SV to a
/// single flattened BTOR2 here, then hand it to `cegar_refine_loop` (the
/// predicate-cube lift operates on one transition system, so the
/// flattened single-BTOR2 shape — not the per-module split — is the
/// right input for the cube). Thin wrapper over [`run_sv_flatten_btor2`];
/// for multi-module *composition* (not single-system CEGAR) use
/// [`translate_sv_per_module`] instead.
pub fn sv_to_btor2(content: &str, yopts: &YosysOptions) -> Result<String, AdapterError> {
    run_sv_flatten_btor2(content, yopts).map(|a| a.btor2)
}

/// Like [`sv_to_btor2`] but additionally returns the names of modules the lift
/// could **not model** because they were instantiated with no body in the
/// source set. Two flavours, both reported:
///
/// 1. Modules Yosys auto-declared `(* blackbox *)`, whose outputs
///    `cutpoint -blackbox` replaced with free inputs.
/// 2. **Undefined-module cells** — an instance whose module type has no
///    definition (e.g. OpenTitan's `prim_sparse_fsm_flop`, instantiated by the
///    `PRIM_FLOP_SPARSE_FSM` macro). Yosys leaves these as dangling cell
///    references (not blackbox *modules*), so `flatten` cannot inline them and
///    any register they drive vanishes from the lifted model entirely.
///
/// Both are sound (an unmodeled driver becomes free / undefined, never a
/// fabricated definite value), but a register hidden behind one is **not**
/// modeled as a state cell, so it cannot be abstracted or verified.
/// [`verify_auto`](crate::adapter::slang::verify_auto) surfaces this list as a
/// diagnostic so the cut is visible rather than silent, and the user can
/// provide the missing module source.
pub fn sv_to_btor2_with_blackboxes(
    content: &str,
    yopts: &YosysOptions,
) -> Result<SvLiftResult, AdapterError> {
    let artifacts = run_sv_flatten_btor2(content, yopts)?;
    let mut names: std::collections::BTreeSet<String> =
        parse_blackbox_modules(&artifacts.hier_json)
            .into_iter()
            .map(|b| b.name)
            .collect();
    names.extend(parse_undefined_module_cells(&artifacts.hier_json));
    Ok(SvLiftResult {
        btor2: artifacts.btor2,
        blackboxed_modules: names.into_iter().collect(),
        frontend_used: artifacts.frontend_used,
        fallback_reason: artifacts.fallback_reason,
    })
}

/// The SV → BTOR2 lift result: the BTOR2 text, the modules the lift could not
/// model (blackboxed / undefined cells), and — for `--frontend auto` — which
/// concrete front end produced it and whether it was a fallback (#466).
pub struct SvLiftResult {
    pub btor2: String,
    pub blackboxed_modules: Vec<String>,
    /// The concrete front end used (`Verilog`/`Slang`, never `Auto`).
    pub frontend_used: SvFrontend,
    /// `Some(reason)` when `Auto` fell back after an earlier front end errored.
    pub fallback_reason: Option<String>,
}

/// Scan the pre-flatten hierarchy JSON for cells whose `type` references a
/// module with no definition in the design (instantiated, no body) and is not a
/// Yosys built-in (`$...`) cell. These dangling cell references are NOT declared
/// blackbox *modules*, so [`parse_blackbox_modules`] misses them, yet they are
/// the dominant real-world "cut FSM" cause (OpenTitan's `prim_sparse_fsm_flop`).
/// Names are de-mangled of sv2v's parameter-specialisation suffix for
/// readability and returned sorted + deduped.
fn parse_undefined_module_cells(hier_json: &str) -> Vec<String> {
    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(modules) = root.get("modules").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let defined: std::collections::HashSet<&str> = modules.keys().map(String::as_str).collect();
    let mut undefined: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for body in modules.values() {
        let Some(cells) = body.get("cells").and_then(|c| c.as_object()) else {
            continue;
        };
        for cell in cells.values() {
            if let Some(ty) = cell.get("type").and_then(|t| t.as_str())
                && !ty.starts_with('$')
                && !defined.contains(ty)
            {
                undefined.insert(strip_sv2v_mangle(ty).to_string());
            }
        }
    }
    undefined.into_iter().collect()
}

/// Strip sv2v's parameter-specialisation suffix (`_<HEX>_<HEX>`, two trailing
/// uppercase-hex groups of ≥ 4 chars) from a mangled module name, so e.g.
/// `prim_sparse_fsm_flop_B4CDB_707CE` reads as `prim_sparse_fsm_flop` in
/// diagnostics. Conservative: only strips when both trailing groups match the
/// exact sv2v shape; otherwise returns the name unchanged.
fn strip_sv2v_mangle(name: &str) -> &str {
    let is_hex4 = |s: &str| s.len() >= 4 && s.chars().all(|c| c.is_ascii_hexdigit());
    if let Some((head, last)) = name.rsplit_once('_')
        && is_hex4(last)
        && let Some((stem, mid)) = head.rsplit_once('_')
        && is_hex4(mid)
        && !stem.is_empty()
    {
        return stem;
    }
    name
}

/// One submodule's translated output, as returned by
/// [`translate_sv_per_module`]. The `btor2_path` is `Some(..)` when
/// `yopts.per_module_output_dir` was set (the BTOR2 file persists);
/// `None` when the per-call tempdir is in use.
#[derive(Debug, Clone)]
pub struct PerModuleOutput {
    pub module_name: String,
    pub btor2_path: Option<PathBuf>,
    pub output: AdapterOutput,
}

/// Translate a SystemVerilog design into one [`AdapterOutput`] *per
/// submodule*, with Yosys's `hierarchy -check` preserving module
/// boundaries and no `flatten` pass running anywhere on the path. This
/// is the R.0b KMTS-pipeline frontend: each submodule reachable from
/// the top is emitted as a separate BTOR2 file, then read back through
/// the existing BTOR2 adapter to produce a per-submodule
/// `AdapterOutput`.
///
/// Strategy: two-pass invocation of Yosys.
///
/// 1. **Discovery pass.** Run Yosys with the user's `read_verilog`
///    sources + `hierarchy -check -top <top>` + `proc` + `write_json`.
///    This produces a hierarchy snapshot from which the submodule list
///    is enumerated. The pass runs even when there is only one module
///    in the design (returning a singleton list).
/// 2. **Per-module emission pass.** For each submodule, run Yosys
///    again, this time with `hierarchy -check -top <m>` (no
///    `flatten`), then the same `async2sync; chformal -lower;
///    dffunmap; setundef …; write_btor` tail as the legacy
///    single-BTOR2 path. Each per-module BTOR2 is fed through the
///    BTOR2 adapter to produce an `AdapterOutput`.
///
/// The cost (~N Yosys invocations for an N-submodule design, each re-reading
/// the whole source set) is acceptable for the small multi-module fixtures the
/// M.0–M.4 validation milestones target, but blows up on a large real design —
/// each Yosys call is therefore capped at [`PER_MODULE_YOSYS_TIMEOUT`] and fails
/// with an actionable error rather than hanging indefinitely. A single-invocation
/// variant using Yosys `select` is feasible in principle but introduces
/// select-scope subtleties that the two-pass shape avoids.
pub fn translate_sv_per_module(
    content: &str,
    options: &AdapterOptions,
    yopts: &YosysOptions,
) -> Result<Vec<PerModuleOutput>, AdapterError> {
    translate_sv_per_module_impl(content, options, yopts, /*surface_net_driving=*/ false)
        .map(|(outputs, _hier)| outputs)
}

/// Per-module port-direction map: `module type → (port → direction)`.
pub type PortDirectionsPerModule =
    HashMap<String, HashMap<String, crate::controllability::BoundaryDirection>>;

/// R-MM — Like [`translate_sv_per_module`] but additionally returns the
/// top module's instance connectivity + per-module port directions, parsed
/// from the same discovery-pass hierarchy snapshot the per-module emission
/// already produces (no extra Yosys invocation). This is the netlist input
/// the KMTS multi-module composition driver (R-MM-4d) needs to wire the
/// per-module KMTSes together: it pairs each [`PerModuleOutput`] (keyed by
/// module *type*) with the [`InstanceConnections`] (keyed by *instance*,
/// carrying the port→net map) and the directions (so each connected port is
/// classified as a reader input vs a driver output).
///
/// Unlike the plain entry, this path **surfaces net-driving combinational
/// outputs** (R-MM-4b) in every per-module lift — the driver needs those
/// output values to synthesise the `<net>_<v>` rendezvous labels. The
/// surface set is computed from the connectivity + directions (output ports
/// that drive a connected net), so single-module designs / parse misses
/// surface nothing.
pub fn translate_sv_per_module_with_connectivity(
    content: &str,
    options: &AdapterOptions,
    yopts: &YosysOptions,
) -> Result<
    (
        Vec<PerModuleOutput>,
        Vec<InstanceConnections>,
        PortDirectionsPerModule,
    ),
    AdapterError,
> {
    let (outputs, hier_body) =
        translate_sv_per_module_impl(content, options, yopts, /*surface_net_driving=*/ true)?;
    let connectivity = parse_instance_connections(&hier_body, yopts.top.as_deref());
    let directions = parse_port_directions_per_module(&hier_body, yopts.top.as_deref());
    Ok((outputs, connectivity, directions))
}

/// R-MM-4d — Output ports that drive a connected net: a port `P` of module
/// type `M` such that `direction(M, P) == Output` and `P` appears in some
/// instance-of-`M`'s `port_to_net`. These are surfaced as per-state
/// valuations (R-MM-4b) so the driver can synthesise their rendezvous
/// labels.
fn net_driving_output_ports(
    connectivity: &[InstanceConnections],
    directions: &PortDirectionsPerModule,
) -> Vec<String> {
    use crate::controllability::BoundaryDirection;
    let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
    for inst in connectivity {
        let Some(dirs) = directions.get(&inst.module_type) else {
            continue;
        };
        for port in inst.port_to_net.keys() {
            if matches!(dirs.get(port), Some(BoundaryDirection::Output)) {
                out.insert(port.clone());
            }
        }
    }
    out.into_iter().collect()
}

/// Shared body for [`translate_sv_per_module`] +
/// [`translate_sv_per_module_with_connectivity`]: runs the two-pass Yosys
/// flow and returns the per-module outputs alongside the raw discovery-pass
/// hierarchy JSON (so the connectivity wrapper can parse it without a
/// second Yosys run). When `surface_net_driving` is set, net-driving output
/// ports (computed from the connectivity + directions in the same hier.json)
/// are surfaced as per-state valuations in every per-module lift.
fn translate_sv_per_module_impl(
    content: &str,
    options: &AdapterOptions,
    yopts: &YosysOptions,
    surface_net_driving: bool,
) -> Result<(Vec<PerModuleOutput>, String), AdapterError> {
    let yosys = locate_yosys()?;
    if !yopts.skip_verific_check {
        verify_no_verific(&yosys)?;
    }

    let tmp = TempDir::new("mununu-yosys-per-module")?;
    let primary = tmp.path().join("work.sv");
    // A2 — strip yosys/slang-rejected net-type qualifiers on port headers
    // (`input tri0 x`) so the design parses; sound (a port net type is invisible
    // to isolated single-module verification). No-op on sources without them.
    write_file(&primary, &strip_port_net_types(content))?;

    let mut sources = vec![primary.clone()];
    for (name, src) in &yopts.additional_sources {
        let p = tmp.path().join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        write_file(&p, &strip_port_net_types(src))?;
        sources.push(p);
    }

    // Optional sv2v preprocessing. Same shape as `translate_sv`.
    if yopts.use_sv2v {
        let preprocessed = tmp.path().join("preprocessed.sv");
        let include_dirs: Vec<PathBuf> = yopts
            .primary_source_path
            .as_ref()
            .and_then(|p| {
                let abs = Path::new(p)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(p));
                abs.parent().map(Path::to_path_buf)
            })
            .into_iter()
            .collect();
        preprocess_sv(&sources, &include_dirs, &preprocessed)?;
        sources = vec![preprocessed];
    }

    // Pass 1 — discovery. Use `hierarchy -auto-top` when the caller did
    // not provide one (matches the legacy build_script).
    let hier_json_path = tmp.path().join("hier.json");
    let discovery_script = build_discovery_script(
        &sources,
        yopts.top.as_deref(),
        &hier_json_path,
        &yopts.params,
    );
    run_yosys(&yosys, &discovery_script, PER_MODULE_YOSYS_TIMEOUT)?;

    let hier_body = std::fs::read_to_string(&hier_json_path).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: per-module discovery pass succeeded but hierarchy snapshot {} is unreadable ({e})",
            hier_json_path.display()
        ),
        location: None,
    })?;
    let submodules = enumerate_submodules(&hier_body, yopts.top.as_deref());
    if submodules.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: "adapter/yosys: per-module emission found no non-blackbox modules in the hierarchy snapshot".into(),
            location: None,
        });
    }

    // Per-module port directions: parse the discovery snapshot once;
    // every submodule's directions live in the same JSON document.
    let top_directions_per_module =
        parse_port_directions_per_module(&hier_body, yopts.top.as_deref());

    // R-MM-4d — when surfacing for composition, compute the net-driving
    // output ports from the connectivity + directions (same hier.json) so
    // each per-module lift surfaces them (R-MM-4b) for the driver's
    // rendezvous-label synthesis.
    let surface_outputs: Vec<String> = if surface_net_driving {
        let connectivity = parse_instance_connections(&hier_body, yopts.top.as_deref());
        net_driving_output_ports(&connectivity, &top_directions_per_module)
    } else {
        Vec::new()
    };

    // Choose output directory for the per-submodule BTOR2 files.
    let out_dir = match &yopts.per_module_output_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(io_err)?;
            dir.clone()
        }
        None => tmp.path().to_path_buf(),
    };
    let persist = yopts.per_module_output_dir.is_some();

    // Pass 2 — per-submodule emission. For each module, run Yosys with
    // `hierarchy -top <m>` (no flatten) and emit `<m>.btor2`. Feed each
    // BTOR2 through the existing BTOR2 adapter.
    let mut outputs = Vec::with_capacity(submodules.len());
    for module_name in &submodules {
        let btor_path = out_dir.join(format!("{module_name}.btor2"));
        let script = build_per_module_script(
            &sources,
            module_name,
            &btor_path,
            yopts.setundef_anyseq,
            yopts.setundef_anyconst,
            &yopts.init_policy_overrides,
            &yopts.params,
        );
        run_yosys(&yosys, &script, PER_MODULE_YOSYS_TIMEOUT)?;

        let btor = std::fs::read_to_string(&btor_path).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: per-module pass for '{module_name}' produced no BTOR2 at {} ({e})",
                btor_path.display()
            ),
            location: None,
        })?;

        // Per-module port-direction context (Document B task B1 — same
        // mechanism as `translate_sv`, scoped to each submodule's
        // boundary).
        let mut btor_options = match top_directions_per_module.get(module_name) {
            Some(directions) if !directions.is_empty() => crate::adapter::AdapterOptions {
                port_directions: directions.clone(),
                ..options.clone()
            },
            _ => options.clone(),
        };
        // R-MM-4d — surface net-driving outputs for composition (each
        // module's lift picks up only its own from the design-wide union).
        if !surface_outputs.is_empty() {
            btor_options.surface_output_ports = surface_outputs.clone();
        }

        let mut out =
            super::btor2::Btor2Adapter::translate(&btor, &btor_options).map_err(|mut e| {
                e.message = format!(
                    "adapter/yosys: per-module BTOR2 reader failed for '{module_name}': {}",
                    e.message
                );
                e
            })?;
        out.source_info = SourceInfo {
            format: SourceFormat::SystemVerilog,
            ..out.source_info
        };

        outputs.push(PerModuleOutput {
            module_name: module_name.clone(),
            btor2_path: if persist { Some(btor_path) } else { None },
            output: out,
        });
    }

    Ok((outputs, hier_body))
}

/// Build the `chparam -set <name> <value> <module>` passes for the requested
/// parameter overrides, to run immediately BEFORE `hierarchy` (so overrides size
/// the design before elaboration). Each `(lhs, value)` is a bare `NAME` (→ the
/// `top` module) or a scoped `MODULE.NAME` (→ that module). Integer values pass
/// through; anything else is emitted as a quoted string literal. Returns the
/// passes joined with `; ` plus a trailing `; ` (so it prefixes the hierarchy
/// pass), or "" when there are no params.
///
/// A bare `NAME` with no `top` (auto-top) emits `chparam -set NAME VALUE` with no
/// module selection — yosys applies it to whichever module carries the parameter,
/// and ERRORS ("Can't find object for defparam") if none does. That error
/// propagates out of the lift (never a silent drop). Naming a `MODULE.NAME` or
/// passing `--top` makes the target explicit.
fn build_chparam_passes(params: &[(String, String)], top: Option<&str>) -> String {
    if params.is_empty() {
        return String::new();
    }
    let passes: Vec<String> = params
        .iter()
        .map(|(lhs, value)| {
            let (module, name) = match lhs.split_once('.') {
                Some((m, n)) => (Some(m), n),
                None => (top, lhs.as_str()),
            };
            // Integer values pass through; anything else is a quoted string literal.
            let val = if value.parse::<i64>().is_ok() {
                value.clone()
            } else {
                format!("\"{}\"", value.replace('"', "\\\""))
            };
            match module {
                Some(m) => format!("chparam -set {name} {val} {m}"),
                None => format!("chparam -set {name} {val}"),
            }
        })
        .collect();
    format!("{}; ", passes.join("; "))
}

/// Yosys discovery-pass script: read sources, set up hierarchy,
/// elaborate processes, dump hierarchy snapshot. Stops before
/// `flatten` / `async2sync` because the per-module emission pass
/// repeats those steps with a different top.
fn build_discovery_script(
    sources: &[PathBuf],
    top: Option<&str>,
    hier_json_out: &Path,
    params: &[(String, String)],
) -> String {
    let read_cmds: Vec<String> = sources
        .iter()
        .map(|p| format!("read_verilog -formal -sv {}", p.display()))
        .collect();
    let chparam = build_chparam_passes(params, top);
    let hier = match top {
        Some(t) => format!("{chparam}hierarchy -check -top {t}"),
        None => format!("{chparam}hierarchy -check -auto-top"),
    };
    format!(
        "{}; {hier}; proc; write_json {}",
        read_cmds.join("; "),
        hier_json_out.display()
    )
}

/// Per-submodule emission script. Runs the same chain as `build_script`
/// but with `<m>` as the top — *each submodule is treated as a
/// self-contained design here*. Internal `flatten` is applied within
/// each submodule's scope because BTOR2 has no notion of sub-module
/// references: any `$paramod` parametrised cells the submodule
/// instantiates must be inlined into the emitted BTOR2.
///
/// The "no flatten" rule of the KMTS-pipeline frontend applies at the
/// *discovery* boundary (so per-submodule enumeration sees real module
/// boundaries) and at *cross-submodule composition* (driven by the
/// top-level netlist, not by Yosys). Within a single submodule's BTOR2,
/// flatten is the correct call.
fn build_per_module_script(
    sources: &[PathBuf],
    module: &str,
    btor_out: &Path,
    setundef_anyseq: bool,
    setundef_anyconst: bool,
    init_policy_overrides: &InitPolicyOverrides,
    params: &[(String, String)],
) -> String {
    let read_cmds: Vec<String> = sources
        .iter()
        .map(|p| format!("read_verilog -formal -sv {}", p.display()))
        .collect();
    let setundef_pass = select_setundef_pass(setundef_anyseq, setundef_anyconst);
    let per_signal = emit_init_policy_setattrs(init_policy_overrides);
    // Parameter overrides apply to this submodule's own top (`module`) before its
    // `hierarchy` sizes it.
    let chparam = build_chparam_passes(params, Some(module));
    format!(
        // `memory_collect` normalizes memory read/write accesses into `$mem` cells
        // (fixing an internal yosys assert on some fifo styles when a raw `$mem`
        // reaches `write_btor`) while KEEPING the memory as a BTOR2 array — the
        // bit-blaster resolves array sorts, so this stays scalable (no map-to-flops
        // explosion). Unlike `memory`/`memory -nomap`, it runs NO internal `opt_clean`,
        // so it does not drop dead registers (which would change memory-free lifts).
        // No-op when the design has no memories.
        "{}; {chparam}hierarchy -check -top {module}; {per_signal}proc; memory_collect; flatten; async2sync; chformal -lower; dffunmap; {setundef_pass}; write_btor {}",
        read_cmds.join("; "),
        btor_out.display()
    )
}

/// R-Y1 (§Phase 8) — Three-way precedence for the Yosys `setundef`
/// pass selection. `anyseq` wins over `anyconst` (strictly more
/// permissive); `anyconst` wins over the default `zero`.
///
/// Trade-off table per §Phase 8 §8.1:
///
/// ```text
///                  | extra state cells       | bug-bearing semantics
/// -----------------+-------------------------+--------------------------
/// `setundef -zero` | 0                       | NO (silently masks CWE-1245-class)
/// `-anyconst`      | 0 (constant inputs)     | YES (init nondeterminism)
/// `-anyseq`        | N per cycle per undef   | YES (per-cycle nondeterminism)
/// ```
///
/// **Default**: `-zero` (preserves historical behaviour; existing
/// fixtures' verdicts unchanged).
fn select_setundef_pass(anyseq: bool, anyconst: bool) -> &'static str {
    match (anyseq, anyconst) {
        (true, _) => "setundef -anyseq",
        (false, true) => "setundef -anyconst",
        (false, false) => "setundef -zero",
    }
}

/// Spawn Yosys with the given script. Captures stdout/stderr into the
/// `AdapterError` message on failure.
fn run_yosys(yosys: &Path, script: &str, timeout: Duration) -> Result<(), AdapterError> {
    let mut command = Command::new(yosys);
    command.arg("-q").arg("-p").arg(script);
    let outcome = crate::adapter::run_with_timeout(&mut command, None, timeout).map_err(|e| {
        AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/yosys: failed to spawn yosys: {e}"),
            location: None,
        }
    })?;
    yosys_run_outcome_to_result(outcome, script, timeout)
}

/// Map a [`crate::adapter::run_with_timeout`] outcome to a result: `None`
/// (timed out ⇒ killed) becomes an actionable scope error rather than a hang;
/// a non-zero exit surfaces yosys's own stderr. Split out so the error paths are
/// unit-testable without spawning a slow subprocess.
/// mununu#504 — map a bounded lift-subprocess outcome, splitting the TIMEOUT case out from the
/// normal one. Pure (`Option<(ExitStatus, String, String)>` in, `Result` out) so the timeout path
/// is unit-testable without spawning anything — the same shape as
/// [`yosys_run_outcome_to_result`], for the same reason.
fn lift_timeout_outcome(
    outcome: Option<(std::process::ExitStatus, String, String)>,
    tool: &str,
) -> Result<(std::process::ExitStatus, String, String), AdapterError> {
    outcome.ok_or_else(|| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: `{tool}` exceeded the lift timeout and was killed. The lift runs \
             before any property is attempted, so this would otherwise hang with no output at \
             all. Raise or disable the cap with {}=<ms> (0 disables), or reduce the design \
             (fewer sources, a narrower --top).",
            crate::adapter::LIFT_TIMEOUT_ENV
        ),
        location: None,
    })
}

fn yosys_run_outcome_to_result(
    outcome: Option<(std::process::ExitStatus, String, String)>,
    script: &str,
    timeout: Duration,
) -> Result<(), AdapterError> {
    // Timed out — killed. Per-module emission targets small fixtures; a large
    // design can thrash past the cap. Surface an actionable error, not a hang.
    let Some((status, stdout, stderr)) = outcome else {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: a per-module yosys invocation exceeded {}s and was killed. \
                 Per-module BTOR2 emission runs one full yosys elaboration PER submodule and is \
                 built for SMALL multi-module fixtures; a large design can exhaust time or memory. \
                 For a large design use the whole-design path (`mununu sv verify-auto`, or a single \
                 `write_btor`) instead of `sv emit-btor2-per-module`.",
                timeout.as_secs()
            ),
            location: None,
        });
    };
    if !status.success() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: yosys exited with status {status} (script: {script})\nstdout:\n{stdout}\nstderr:\n{stderr}"
            ),
            location: None,
        });
    }
    Ok(())
}

/// Enumerate submodule names from a Yosys `write_json` hierarchy
/// snapshot. Returns module names in deterministic (sorted) order,
/// excluding the top module and any module flagged `(* blackbox *)`
/// (blackbox bodies are not extractable to BTOR2).
///
/// When `explicit_top` is provided, it is excluded from the result.
/// When `explicit_top` is `None`, the function attempts to discover
/// the top via the `(* top *)` attribute and excludes it. If no top
/// is identifiable, every non-blackbox module is returned (the caller
/// can decide what to do).
pub fn enumerate_submodules(hier_json: &str, explicit_top: Option<&str>) -> Vec<String> {
    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let modules = match root.get("modules").and_then(|m| m.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    // Determine which name to exclude as the top.
    let auto_top: Option<String> = modules.iter().find_map(|(name, body)| {
        let is_top = body
            .get("attributes")
            .and_then(|a| a.get("top"))
            .map(|v| v.as_str().is_some_and(|s| s.ends_with('1')))
            .unwrap_or(false);
        if is_top { Some(name.clone()) } else { None }
    });
    let top_name = explicit_top.map(str::to_string).or(auto_top);

    let mut out: Vec<String> = modules
        .iter()
        .filter(|(name, body)| {
            if Some(name.as_str()) == top_name.as_deref() {
                return false;
            }
            let is_bb = body
                .get("attributes")
                .and_then(|a| a.get("blackbox"))
                .map(|v| v.as_str().is_some_and(|s| s.ends_with('1')))
                .unwrap_or(false);
            if is_bb {
                return false;
            }
            // Yosys mangles parameterised module instances as
            // `$paramod$<hash>\<original_name>`. These represent a
            // specific parameter elaboration of a generic module and
            // cannot be re-elaborated as `hierarchy -top` from the
            // original source — the parameter context is lost outside
            // the instantiating scope. Skip them; the parent wrapper's
            // BTOR2 already contains the elaborated logic (Yosys's
            // `write_btor` inlines parameterised cells because BTOR2
            // has no notion of sub-module references).
            !name.starts_with("$paramod")
        })
        .map(|(name, _)| name.clone())
        .collect();
    out.sort();

    // If there are no non-top non-blackbox modules, fall back to
    // emitting just the top — designs with a single module (the
    // typical hand-written fixture) still produce one BTOR2 file.
    if out.is_empty()
        && let Some(top) = top_name
        && modules.contains_key(&top)
    {
        out.push(top);
    }

    out
}

/// Parse port directions for *every* module in the hierarchy snapshot.
/// Used by [`translate_sv_per_module`] to pass each submodule's
/// boundary direction map into the BTOR2 adapter (Document B task B1
/// extension to multi-module).
fn parse_port_directions_per_module(
    hier_json: &str,
    _explicit_top: Option<&str>,
) -> HashMap<String, HashMap<String, crate::controllability::BoundaryDirection>> {
    use crate::controllability::BoundaryDirection;

    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let modules = match root.get("modules").and_then(|m| m.as_object()) {
        Some(m) => m,
        None => return HashMap::new(),
    };

    let mut all = HashMap::new();
    for (module_name, body) in modules {
        let port_map = match body.get("ports").and_then(|p| p.as_object()) {
            Some(p) => p,
            None => continue,
        };
        let mut per_module = HashMap::new();
        for (name, info) in port_map {
            let direction_str = info
                .get("direction")
                .and_then(|d| d.as_str())
                .unwrap_or("input");
            let direction = match direction_str {
                "input" => BoundaryDirection::Input,
                "output" => BoundaryDirection::Output,
                "inout" => BoundaryDirection::Inout,
                _ => BoundaryDirection::Internal,
            };
            per_module.insert(name.clone(), direction);
        }
        all.insert(module_name.clone(), per_module);
    }
    all
}

/// R-MM — Connectivity of one submodule instance inside the top module,
/// with each port resolved to the connected net NAME.
///
/// The KMTS multi-module composition driver (R-MM-4) uses this to rename
/// each instance's per-module port labels to the *parent net* names, so
/// two instances sharing a net rendezvous under
/// [`crate::composition::compose`] (whose synchronisation is name-equality
/// on label payloads). Example from `producer_consumer_top`: the producer
/// drives net `valid`, the buffer reads it as port `push`; both resolve to
/// net `valid` here, so after relabelling they synchronise on `valid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceConnections {
    /// Cell instance name in the parent (e.g. `u_producer`).
    pub instance: String,
    /// Instantiated submodule type (e.g. `producer`). May be a Yosys
    /// `$paramod$…` mangled name for parameterised instances; the driver
    /// resolves that to the source module.
    pub module_type: String,
    /// Port name on the instance → connected net name in the parent. Only
    /// ports whose bit-vector resolves to a *named* net appear here;
    /// constant-tied or sub-ranged ports are omitted (they do not
    /// participate in net-name rendezvous).
    pub port_to_net: std::collections::BTreeMap<String, String>,
}

/// R-MM — Parses the top module's instance connectivity from a Yosys
/// hierarchy JSON snapshot (the `write_json` artifact captured *before*
/// `flatten` in the per-module discovery pass), resolving every instance
/// port to the connected net NAME via the top module's `netnames` table.
///
/// The top is `explicit_top` when given, else the module flagged
/// `attributes.top`. Returns an empty Vec on parse failure / missing top
/// — best-effort, mirroring [`enumerate_submodules`].
pub fn parse_instance_connections(
    hier_json: &str,
    explicit_top: Option<&str>,
) -> Vec<InstanceConnections> {
    use std::collections::BTreeMap;

    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let modules = match root.get("modules").and_then(|m| m.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    // Resolve the top (explicit, else attributes.top ends with '1').
    let auto_top: Option<String> = modules.iter().find_map(|(name, body)| {
        let is_top = body
            .get("attributes")
            .and_then(|a| a.get("top"))
            .map(|v| v.as_str().is_some_and(|s| s.ends_with('1')))
            .unwrap_or(false);
        if is_top { Some(name.clone()) } else { None }
    });
    let top_name = match explicit_top.map(str::to_string).or(auto_top) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let top = match modules.get(&top_name).and_then(|m| m.as_object()) {
        Some(t) => t,
        None => return Vec::new(),
    };

    // Net-bits → net-name reverse map from the top's `netnames` table.
    let mut bits_to_net: HashMap<Vec<String>, String> = HashMap::new();
    if let Some(netnames) = top.get("netnames").and_then(|n| n.as_object()) {
        for (net_name, info) in netnames {
            if let Some(bits) = info.get("bits").and_then(|b| b.as_array()) {
                let key = normalize_net_bits(bits);
                if !key.is_empty() {
                    // First writer wins (deterministic under preserve_order)
                    // when a net carries multiple alias names.
                    bits_to_net.entry(key).or_insert_with(|| net_name.clone());
                }
            }
        }
    }

    // Walk each instance's port connections; resolve bits → net name.
    let cells = match top.get("cells").and_then(|c| c.as_object()) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for (instance, body) in cells {
        let module_type = body
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let mut port_to_net: BTreeMap<String, String> = BTreeMap::new();
        if let Some(conns) = body.get("connections").and_then(|c| c.as_object()) {
            for (port, bits_val) in conns {
                if let Some(bits) = bits_val.as_array()
                    && let Some(net) = bits_to_net.get(&normalize_net_bits(bits))
                {
                    port_to_net.insert(port.clone(), net.clone());
                }
            }
        }
        out.push(InstanceConnections {
            instance: instance.clone(),
            module_type,
            port_to_net,
        });
    }
    out.sort_by(|a, b| a.instance.cmp(&b.instance));
    out
}

/// Normalises a Yosys JSON bit array — elements are net-id integers or
/// constant strings (`"0"`/`"1"`/`"x"`/`"z"`) — to a canonical
/// `Vec<String>` usable as a net-identity map key. The same normalisation
/// is applied to `netnames` bits and cell-connection bits so they compare
/// equal.
fn normalize_net_bits(bits: &[serde_json::Value]) -> Vec<String> {
    bits.iter()
        .map(|b| {
            if let Some(n) = b.as_i64() {
                n.to_string()
            } else if let Some(s) = b.as_str() {
                s.to_string()
            } else {
                b.to_string()
            }
        })
        .collect()
}

/// Parse the yosys `write_json` output and extract any modules with the
/// `(* blackbox *)` attribute as `BlackBoxInterface` records. Returns an
/// empty Vec on parse failures — the sidecar emission is best-effort
/// and should not fail the whole adapter run if yosys's JSON schema
/// drifts under us.
///
/// Yosys's JSON layout (per `yosys -p 'write_json --help'`):
///
/// ```text
/// {
///   "modules": {
///     "<name>": {
///       "attributes": { "blackbox": "00…001", "src": "foo.sv:10.1-..." },
///       "ports":      { "<port>": { "direction": "input|output|inout", "bits": [...] } },
///       …
///     }
///   }
/// }
/// ```
fn parse_blackbox_modules(hier_json: &str) -> Vec<crate::contract::discover::BlackBoxInterface> {
    use crate::contract::discover::{BlackBoxInterface, PortDescriptor};
    use crate::controllability::BoundaryDirection;

    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let modules = match root.get("modules").and_then(|m| m.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for (name, body) in modules {
        let attrs = body.get("attributes").and_then(|a| a.as_object());
        let is_blackbox = attrs
            .and_then(|a| a.get("blackbox"))
            .map(|v| {
                // yosys serialises bool attributes as bitstrings; any
                // string ending in '1' counts as set.
                v.as_str().is_some_and(|s| s.ends_with('1'))
            })
            .unwrap_or(false);
        if !is_blackbox {
            continue;
        }

        // Pull ports in declaration order. yosys preserves order in
        // serde_json::Map under the `preserve_order` feature; without
        // that feature the iteration is undefined. We sort the port
        // names alphabetically as a deterministic fallback — the
        // adapter's tests in §B.7.3 / §B.8 do not depend on port
        // declaration order.
        let mut ports: Vec<PortDescriptor> = Vec::new();
        if let Some(port_map) = body.get("ports").and_then(|p| p.as_object()) {
            let mut names: Vec<&String> = port_map.keys().collect();
            names.sort();
            for port_name in names {
                let port_body = match port_map.get(port_name) {
                    Some(b) => b,
                    None => continue,
                };
                let direction_str = port_body
                    .get("direction")
                    .and_then(|d| d.as_str())
                    .unwrap_or("input");
                let direction = match direction_str {
                    "input" => BoundaryDirection::Input,
                    "output" => BoundaryDirection::Output,
                    "inout" => BoundaryDirection::Inout,
                    _ => BoundaryDirection::Internal,
                };
                ports.push(PortDescriptor {
                    name: port_name.clone(),
                    direction,
                    description: None,
                });
            }
        }

        // Source location, parsed from yosys's `src` attribute when
        // present. yosys's format is e.g. `"foo.sv:10.1-25.6"` —
        // we keep only the filename and the first line number.
        let (source_file, source_line) = attrs
            .and_then(|a| a.get("src"))
            .and_then(|v| v.as_str())
            .map(parse_yosys_src)
            .unwrap_or((None, None));

        // Document D task D4 + Document A task A6: collect any
        // `mununu_*` annotations yosys preserved in the attribute
        // map, so the auto-emitted `BlackBoxInterface` carries the
        // vendor-supplied A/G clauses + interface URIs along with
        // the bare port list.
        let annotations = attrs
            .map(|a| {
                crate::mununu_annotations::extract_from_yosys_attributes(
                    &serde_json::Value::Object(a.clone()),
                )
            })
            .unwrap_or_default();

        out.push(BlackBoxInterface {
            name: name.clone(),
            ports,
            source_file,
            source_line,
            annotations,
        });
    }
    // Deterministic order for downstream consumers.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Extract the top module's port directions from yosys's pre-flatten
/// `write_json` output. Returns an empty map when the JSON cannot be
/// parsed, the top module cannot be identified, or it has no ports.
///
/// Identifies the top module by:
///   1. Explicit `top` argument when provided (matches what `hierarchy -top X`
///      saw).
///   2. Otherwise the single module with the `attributes.top = 1` attribute
///      that yosys emits when running `hierarchy -auto-top`.
///   3. Otherwise the first non-blackbox module by sorted name (deterministic
///      fallback).
///
/// Used by Document B task B1 — feeding the §4 controllability rule
/// (port direction → controllability) into the BTOR2 reader for top-
/// module inputs.
fn parse_top_module_port_directions(
    hier_json: &str,
    explicit_top: Option<&str>,
) -> std::collections::HashMap<String, crate::controllability::BoundaryDirection> {
    use crate::controllability::BoundaryDirection;
    use std::collections::HashMap;

    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let modules = match root.get("modules").and_then(|m| m.as_object()) {
        Some(m) => m,
        None => return HashMap::new(),
    };

    let top_name: String = match explicit_top {
        Some(t) if modules.contains_key(t) => t.to_string(),
        _ => {
            let mut auto_top: Option<&String> = None;
            for (name, body) in modules {
                let is_top = body
                    .get("attributes")
                    .and_then(|a| a.get("top"))
                    .map(|v| v.as_str().is_some_and(|s| s.ends_with('1')))
                    .unwrap_or(false);
                if is_top {
                    auto_top = Some(name);
                    break;
                }
            }
            if let Some(name) = auto_top {
                name.clone()
            } else {
                // Pick the first non-blackbox module deterministically.
                let mut candidates: Vec<&String> = modules
                    .iter()
                    .filter(|(_, body)| {
                        let is_bb = body
                            .get("attributes")
                            .and_then(|a| a.get("blackbox"))
                            .map(|v| v.as_str().is_some_and(|s| s.ends_with('1')))
                            .unwrap_or(false);
                        !is_bb
                    })
                    .map(|(name, _)| name)
                    .collect();
                candidates.sort();
                match candidates.first() {
                    Some(name) => (*name).clone(),
                    None => return HashMap::new(),
                }
            }
        }
    };

    let body = match modules.get(&top_name) {
        Some(b) => b,
        None => return HashMap::new(),
    };
    let port_map = match body.get("ports").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return HashMap::new(),
    };

    let mut out = HashMap::new();
    for (name, info) in port_map {
        let direction_str = info
            .get("direction")
            .and_then(|d| d.as_str())
            .unwrap_or("input");
        let direction = match direction_str {
            "input" => BoundaryDirection::Input,
            "output" => BoundaryDirection::Output,
            "inout" => BoundaryDirection::Inout,
            _ => BoundaryDirection::Internal,
        };
        out.insert(name.clone(), direction);
    }
    out
}

/// Best-effort parse of yosys's `src` attribute into `(file, line)`.
/// Examples that should round-trip:
/// - `"foo.sv:10.1-25.6"` → `("foo.sv", 10)`
/// - `"path/with:colon/foo.sv:5"` → `("path/with:colon/foo.sv", 5)` (last `:` before line)
fn parse_yosys_src(src: &str) -> (Option<String>, Option<u32>) {
    let Some(colon) = src.rfind(':') else {
        return (Some(src.to_string()), None);
    };
    let file_part = &src[..colon];
    let line_part = &src[colon + 1..];
    // Strip "<line>.<col>-..." down to just the line.
    let line_only = line_part.split(['.', '-']).next().unwrap_or(line_part);
    let line: Option<u32> = line_only.parse().ok();
    (Some(file_part.to_string()), line)
}

/// Find a usable yosys binary.
fn locate_yosys() -> Result<PathBuf, AdapterError> {
    if let Ok(path) = std::env::var("MUNUNU_YOSYS_PATH") {
        return Ok(PathBuf::from(path));
    }
    let candidates = ["yosys"];
    for c in candidates {
        if let Ok(out) = Command::new(c).arg("-V").output()
            && out.status.success()
        {
            return Ok(PathBuf::from(c));
        }
    }
    Err(AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: "adapter/yosys: yosys binary not found in $PATH (set MUNUNU_YOSYS_PATH or install yosys ≥ 0.40)".into(),
        location: None,
    })
}

/// Preprocess one or more SystemVerilog sources through `sv2v` and
/// write the elaborated Verilog-2005 output to `out`. Public entry
/// point for R.0a's standalone `mununu sv preprocess` CLI subcommand
/// and for any future caller that needs sv2v output without
/// invoking Yosys.
///
/// `include_dirs` forwards `-I` flags so `\`include` directives in the
/// `.sv` sources resolve. Returns the absolute path of `sv2v` that
/// was used (for diagnostics) on success; `AdapterError` if the
/// binary is missing or the subprocess fails.
///
/// This wraps the same `locate_sv2v` + `run_sv2v` helpers the Yosys
/// `--preprocessor sv2v` path uses, exposed here as a single public
/// function. Soundness invariant: identical sv2v invocation on both
/// paths means any future change to one path lands on the other for
/// free.
pub fn preprocess_sv(
    sources: &[PathBuf],
    include_dirs: &[PathBuf],
    out: &Path,
) -> Result<PathBuf, AdapterError> {
    let sv2v = locate_sv2v()?;
    run_sv2v(&sv2v, sources, include_dirs, out)?;
    Ok(sv2v)
}

/// Find a usable sv2v binary. Mirrors `locate_yosys` — check
/// `MUNUNU_SV2V_PATH` first, then fall back to the bare `sv2v` on
/// `$PATH`. zachjs/sv2v's `--version` flag is a stable smoke test.
fn locate_sv2v() -> Result<PathBuf, AdapterError> {
    if let Ok(path) = std::env::var("MUNUNU_SV2V_PATH") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(out) = Command::new("sv2v").arg("--version").output()
        && out.status.success()
    {
        return Ok(PathBuf::from("sv2v"));
    }
    Err(AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: "adapter/yosys: sv2v binary not found in $PATH (set MUNUNU_SV2V_PATH or install zachjs/sv2v ≥ 0.0.10). Required by --preprocessor sv2v / MUNUNU_USE_SV2V=1."
            .into(),
        location: None,
    })
}

/// The yosys `-m` argument that loads the yosys-slang frontend plugin (`read_slang`),
/// or `None` if unavailable. Order: `MUNUNU_YOSYS_SLANG_PLUGIN` (a path to `slang.so`) →
/// the pinned `mununu-sva` image plugin (loadable by bare name `slang`) → absent.
///
/// `read_slang` is a full SystemVerilog front-end that accepts constructs yosys's native
/// `read_verilog` rejects (a bounded `while` loop in `always @*`, …). mununu uses it for
/// the **RTL lift only** (`--ignore-assertions` — SVA is extracted separately by the slang
/// `extract_sva` path, so no `bad` nodes are needed here). Named registers and ports get
/// the SAME `instance.signal` BTOR2 names as the `read_verilog` path, so cutpoints /
/// `--config-value` over named signals resolve unchanged.
fn locate_slang_plugin() -> Option<String> {
    if let Ok(p) = std::env::var("MUNUNU_YOSYS_SLANG_PLUGIN")
        && Path::new(&p).exists()
    {
        return Some(p);
    }
    // The pinned image ships slang.so in yosys's plugin dir → loadable by bare name.
    if Path::new("/opt/oss-cad-suite/share/yosys/plugins/slang.so").exists() {
        return Some("slang".to_string());
    }
    None
}

/// The yosys-slang `-m` plugin argument when the slang RTL frontend is selected for this
/// lift, else `None`. Opt-in via `MUNUNU_YOSYS_FRONTEND=slang` (case-insensitive) AND the
/// plugin being present. Conservative rollout (A3): opt-in first; a
/// fallback-on-`read_verilog`-parse-failure and a benchmark-validated default are follow-ups.
fn slang_frontend_selection() -> Option<String> {
    match std::env::var("MUNUNU_YOSYS_FRONTEND") {
        Ok(v) if v.eq_ignore_ascii_case("slang") => locate_slang_plugin(),
        _ => None,
    }
}

/// Run sv2v over `sources`, capture stdout into `out`. sv2v's documented
/// multi-file mode resolves cross-file packages/interfaces in one pass
/// and emits a single combined Verilog-2005 stream on stdout.
///
/// `include_dirs` forwards `-I` flags so `\`include` directives in the
/// `.sv` sources (e.g. `\`include "caliptra_sva.svh"`) resolve. Without
/// these, sv2v errors on missing headers because mununu stages a per-call
/// tempdir away from the original source directory.
fn run_sv2v(
    sv2v: &Path,
    sources: &[PathBuf],
    include_dirs: &[PathBuf],
    out: &Path,
) -> Result<(), AdapterError> {
    let mut cmd = Command::new(sv2v);
    for dir in include_dirs {
        cmd.arg(format!("-I{}", dir.display()));
    }
    cmd.args(sources);
    // mununu#504 — bounded; see the note on the main yosys lift.
    let outcome = match crate::adapter::lift_timeout() {
        Some(t) => crate::adapter::run_with_timeout(&mut cmd, None, t),
        None => cmd.output().map(|o| {
            Some((
                o.status,
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ))
        }),
    }
    .map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("adapter/yosys: failed to spawn sv2v: {e}"),
        location: None,
    })?;
    let (status, stdout_s, stderr_s) = lift_timeout_outcome(outcome, "sv2v")?;
    let result = std::process::Output {
        status,
        stdout: stdout_s.into_bytes(),
        stderr: stderr_s.into_bytes(),
    };
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout = String::from_utf8_lossy(&result.stdout);
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: sv2v (preprocessor) exited with status {} \nstdout:\n{stdout}\nstderr:\n{stderr}",
                result.status
            ),
            location: None,
        });
    }
    std::fs::write(out, &result.stdout).map_err(io_err)
}

/// Best-effort check that the located yosys binary was not built with the
/// commercial Verific frontend. Verific-built yosys binaries print
/// "verific" in their `-V` banner.
fn verify_no_verific(yosys: &Path) -> Result<(), AdapterError> {
    let out = Command::new(yosys)
        .arg("-V")
        .output()
        .map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/yosys: failed to read yosys version: {e}"),
            location: None,
        })?;
    let banner = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if banner.to_lowercase().contains("verific") {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "adapter/yosys: refusing to use a yosys binary built with Verific (commercial / license-incompatible). Banner:\n{banner}"
            ),
            location: None,
        });
    }
    Ok(())
}

// `build_script` (defined further below) elaborates `sources` into a
// BTOR2 file. The pipeline is intentionally **non-pruning** —
// verification benefits
// from observing every register, even those without an external output.
// `prep` and `opt -fast` are explicitly avoided because they strip cells
// not feeding outputs / asserts and would silently produce an empty BTOR2
// for SV without assertions.
//
// Components of `build_script` (defined further below), in order:
//
//  1. `read_verilog -formal -sv` — parse, enabling SVA / formal constructs.
//  2. `hierarchy -top X` — select the design's root.
//  3. `proc` — lower always-blocks to RTL netlist.
//  4. `write_json <hier>` — **before flatten** — capture the elaborated
//     hierarchy (modules, ports with directions, attributes) so the
//     driver can detect `(* blackbox *)` modules and auto-emit
//     `BlackBoxInterface.json` + `GapMarkerReport.json` sidecars
//     (Document B task B2 + B3). Without this snapshot, `flatten`
//     erases module boundaries before the driver gets a chance to see
//     them.
//  5. `flatten` — inline submodule instances.
//  6. `async2sync` — convert async-reset / async-set cells to plain
//     synchronous DFFs while **preserving the synchronous structure**
//     (the clock is implicit in BTOR2's `state` semantics, not exposed
//     as edge-detect combinational logic). This lets `chformal -lower`
//     translate the assertions cleanly without introducing the `value +
//     shadow + previous-clk` triple-state-cell encoding that
//     `clk2fflogic` would. The previous (`clk2fflogic`-based) script
//     produced ~3 state cells per user FF group; this script produces
//     1, matching mununu's "each CLTS transition = one clock edge"
//     semantics natively.
//  7. `chformal -lower` — translate SVA `assert` / `assume` / `cover` into
//     BTOR2 `bad` / `constraint` / fair / `justice` signals. No-op if no
//     SVA is present in the design.
//  8. `dffunmap` — unmap SDFF (synchronous-reset) cells to plain DFF +
//     explicit reset logic (write_btor only accepts plain DFF).
//  9. `setundef -zero` — replace any remaining X / undef bits with 0
//     (deterministic; bit-blaster does not model X-prop).
// 10. `write_btor` — emit the BTOR2.

/// R-Y2 (§Phase 8 §8.1) — Per-signal init-policy override list.
///
/// Each pair is `(signal_name, InitPolicy)`. The Yosys script-builder
/// emits `setattr -mod -set <attr> <val> w:<signal>` for each entry
/// between `read_verilog` and `proc`, giving surgical control over
/// individual signals (e.g. anyconst on `boot_fsm_ns` while other
/// undefs stay zero on the Caliptra fixture).
pub type InitPolicyOverrides = Vec<(
    String,
    crate::adapter::systemverilog::annotation::InitPolicy,
)>;

/// R-Y2 — Emit one Yosys `setattr` command per init-policy override.
/// Returns an empty string when overrides is empty (no-op insertion
/// preserves the legacy script shape).
fn emit_init_policy_setattrs(overrides: &InitPolicyOverrides) -> String {
    let mut commands: Vec<String> = Vec::new();
    for (name, policy) in overrides {
        if let Some((attr, val)) = policy.yosys_attribute() {
            commands.push(format!("setattr -set {attr} {val} w:{name}"));
        }
    }
    if commands.is_empty() {
        String::new()
    } else {
        // Trailing semicolon so the caller can concatenate without
        // worrying about whether overrides are present.
        format!("{}; ", commands.join("; "))
    }
}

/// A Yosys net name is safe to interpolate into the `-p` script iff it is a
/// plain identifier (optionally hierarchical `a.b.c` or bit-selected `sig[3:0]`).
/// This is a security gate: the whole script is one `-p` argument, so a name
/// containing `;`, whitespace, or quotes could inject an arbitrary Yosys pass
/// (OWASP command-injection). Anything else is rejected.
fn is_valid_net_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.' | '[' | ']' | ':'))
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// Emit the explicit control-slice cut-point pass: `cutpoint w:<sig> …; ` (empty
/// when no valid signals). Placed after `proc`/`write_json` and before `flatten`
/// so the named nets still exist unprefixed; `cutpoint` replaces each net's driver
/// with a free `$anyseq`, and cone-of-influence then drops the (now-dead) datapath
/// fanin. Invalid names ([`is_valid_net_name`]) are skipped — a skipped cut point
/// only leaves the cone wider (the property may be `Skipped`), never a wrong verdict.
fn emit_cutpoints(signals: &[String]) -> String {
    let sel: Vec<String> = signals
        .iter()
        .filter(|s| is_valid_net_name(s))
        .map(|s| format!("w:{s}"))
        .collect();
    if sel.is_empty() {
        String::new()
    } else {
        format!("cutpoint {}; ", sel.join(" "))
    }
}

// The lift-script builder threads each independent Yosys-pass knob (top, output
// paths, the two setundef flags, per-signal init overrides, and now the cut points)
// as its own argument; bundling them into a struct would not improve clarity here.
#[allow(clippy::too_many_arguments)]
fn build_script(
    sources: &[PathBuf],
    top: Option<&str>,
    btor_out: &Path,
    hier_json_out: &Path,
    setundef_anyseq: bool,
    setundef_anyconst: bool,
    init_policy_overrides: &InitPolicyOverrides,
    cutpoint_signals: &[String],
    use_slang: bool,
    extra_include_dirs: &[PathBuf],
    params: &[(String, String)],
) -> String {
    // Caller-supplied include-search dirs, as attached `-I<dir>` flags (the
    // form yosys `read_verilog`/`read_slang` and sv2v all accept). Lets
    // `\`include "frag.vh"` resolve against the original source tree without
    // the fragment being read as a standalone compilation unit.
    let extra_inc: String = extra_include_dirs
        .iter()
        .map(|d| format!(" -I{}", d.display()))
        .collect();
    // A3 — the RTL read command. `read_slang` (yosys-slang plugin) is a full SV front-end
    // that accepts constructs `read_verilog` rejects; it reads ALL sources in ONE command
    // and `--ignore-assertions` drops embedded SVA (extracted separately by the slang
    // `extract_sva` path). `read_verilog` reads each source. Everything after the read is
    // identical, so the lift is frontend-agnostic downstream.
    //
    // `--ignore-timing`: slang is stricter than `read_verilog` about `#`-delay timing
    // controls (`error: unsynthesizable timing control`), which `read_verilog` silently
    // drops. Ignoring them here matches `read_verilog`'s behaviour and is the correct
    // cycle-based-verification modelling choice — `#delays` are non-synthesizable and are
    // not part of the design's synthesized (cycle) behaviour. SOUNDNESS: this is
    // read_verilog-equivalent, not an extra abstraction. Without it, slang regressed ~18
    // corpus designs that read_verilog lifts (measured 2026-07-25 lift sweep).
    let read_cmds: Vec<String> = if use_slang {
        let files = sources
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let top_arg = top.map(|t| format!(" --top {t}")).unwrap_or_default();
        // The staging tempdir (parent of the sources) as an include dir, so a cross-file
        // `\`include "peer.sv"` between staged sources resolves; plus any caller dirs.
        let inc = sources
            .first()
            .and_then(|p| p.parent())
            .map(|d| format!(" -I{}", d.display()))
            .unwrap_or_default();
        // Parameter overrides — slang (read_slang) fully ELABORATES at read time, so
        // a post-read `chparam` finds nothing ("not parametric"). The Verilog-style
        // `-G NAME=VALUE` override at read time is the right mechanism here; it is
        // top-scoped, so a `MODULE.NAME` override applies by its bare name. (An
        // unknown name is validated upstream against the slang AST — read_slang
        // itself would silently ignore it.)
        let param_g: String = params
            .iter()
            .map(|(lhs, val)| {
                let name = lhs.rsplit('.').next().unwrap_or(lhs);
                format!(" -G {name}={val}")
            })
            .collect();
        vec![format!(
            "read_slang --ignore-assertions --ignore-timing{top_arg}{param_g}{inc}{extra_inc} {files}"
        )]
    } else {
        sources
            .iter()
            .map(|p| format!("read_verilog -formal -sv{extra_inc} {}", p.display()))
            .collect()
    };
    // chparam is the read_verilog path's parameter-override mechanism (params stay
    // unresolved until `hierarchy`, so a pre-`hierarchy` chparam applies and errors
    // on an unknown name). The slang path uses `-G` at read time instead (above).
    let chparam = if use_slang {
        String::new()
    } else {
        build_chparam_passes(params, top)
    };
    let hier = match top {
        Some(t) => format!("{chparam}hierarchy -top {t}"),
        None => format!("{chparam}hierarchy -auto-top"),
    };
    // `cutpoint -blackbox` between the hierarchy snapshot and `flatten`
    // replaces every output of every `(* blackbox *)` cell with an
    // `$anyseq` (free variable) cell. This is what makes the rest of
    // the pipeline succeed even when the blackbox body is empty — the
    // blackbox's outputs become uncontrollable free signals, exactly
    // the chaotic-stub semantics Document A §2 prescribes.
    // SOUNDNESS — `setundef` three-way trade-off (R-Y1, §Phase 8 §8.1):
    //
    // `setundef` decides how Yosys treats nets that have no assigned
    // value on some path (an `always_comb` case statement with no
    // `default:` arm; an `unique casez` with unmatched encodings).
    //
    // - `-zero` (default) pins them to 0. Deterministic; state space
    //   stays small; **silently de-bugs CWE-1245-class defects**
    //   because the unmatched-case path becomes a deterministic
    //   transition to 0 instead of an admissible-any-value sink.
    //
    // - `-anyconst` introduces one nondeterministic *constant input*
    //   per undef bit (NOT per-cycle state cells). Solver picks any
    //   concrete value at init; value is then fixed for the run.
    //   Preserves CWE-1245-class bug-bearing semantics at zero
    //   extra state-cell cost — for a 3-bit FSM register this is 3
    //   constant inputs ≡ 8 init choices, vs `-anyseq`'s 2^56
    //   inflation on the Caliptra fixture. This is **the
    //   intermediate the Caliptra CWE-1245 fixture has been
    //   waiting for.** Opt in via `YosysOptions::setundef_anyconst`.
    //
    // - `-anyseq` makes them free symbolic choices each cycle.
    //   Preserves the bug-bearing semantics, but introduces fresh
    //   `$anyseq` state cells at each undefined point — the
    //   state-bit count explodes (≈ 56 bits on the Caliptra fixture
    //   vs ≈ 19 bits under `-zero`). For designs near the
    //   `MAX_STATE_BITS` cap, `-anyseq` pushes the design *over*
    //   the cap and the bit-blaster refuses it. Opt in via
    //   `YosysOptions::setundef_anyseq`.
    //
    // Precedence when multiple flags are set: `-anyseq` wins over
    // `-anyconst` (strictly more permissive); `-anyconst` wins over
    // the default `-zero`. mununu defaults to `-zero` (small state
    // space; CWE-1245 hidden unless re-instated). The Phase A.4
    // step 4.6 + §Phase 8 §8.2 documented this trade-off explicitly.
    // Per-signal granularity (anyconst only on selected registers)
    // is R-Y2 (§Phase 8 §8.1), shipping post-R-Y1.
    let setundef_pass = select_setundef_pass(setundef_anyseq, setundef_anyconst);
    let per_signal = emit_init_policy_setattrs(init_policy_overrides);
    // Explicit control-slice cut points, spliced after the discovery `write_json`
    // snapshot (so state-cell discovery still sees the real signals) and before
    // `flatten`/bit-blast: each named net is driven by a free `$anyseq`, and
    // cone-of-influence drops its now-dead datapath fanin. Over-approximation —
    // see `YosysOptions::cutpoint_signals`.
    let cutpoints = emit_cutpoints(cutpoint_signals);
    // `memory_collect` (see the single-module lift above): normalize memory ports
    // for BTOR2 array emission, no opt_clean / map-to-flops. No-op when memory-free.
    // The explicit control-slice cut points splice in just before `cutpoint -blackbox`
    // (both after `write_json`, before `memory_collect`/`flatten`).
    format!(
        "{}; {hier}; {per_signal}proc; write_json {}; {cutpoints}cutpoint -blackbox; memory_collect; flatten; async2sync; chformal -lower; dffunmap; {setundef_pass}; write_btor {}",
        read_cmds.join("; "),
        hier_json_out.display(),
        btor_out.display()
    )
}

fn write_file(path: &Path, content: &str) -> Result<(), AdapterError> {
    std::fs::write(path, content).map_err(io_err)
}

fn io_err(e: std::io::Error) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("adapter/yosys: io error: {e}"),
        location: None,
    }
}

/// Extension method for callers who already have an AdapterOptions and just
/// want the multi-source variant.
pub fn translate_sv_multi(
    primary: &str,
    additional: &HashMap<String, String>,
    options: &AdapterOptions,
    top: Option<&str>,
) -> Result<AdapterOutput, AdapterError> {
    let yopts = YosysOptions {
        top: top.map(str::to_string),
        additional_sources: additional
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        ..Default::default()
    };
    translate_sv(primary, options, &yopts)
}

// =====================================================================
// TempDir helper (avoid pulling in another dep just for this)
// =====================================================================

struct TempDir {
    path: PathBuf,
    keep: bool,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self, AdapterError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        // Per-call uniqueness: pid + monotonic process counter + nanos
        // + thread id. Two parallel tests in the same process must not
        // collide on the same tempdir path.
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
        base.push(format!("{prefix}-{pid}-{tid_digits}-{seq}-{nanos}"));
        std::fs::create_dir_all(&base).map_err(io_err)?;
        let keep = std::env::var("MUNUNU_KEEP_YOSYS_TMP")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Ok(TempDir { path: base, keep })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllability::BoundaryDirection;

    // P1.1 — Auto-frontend fallback attempt ordering (the slang branch itself needs the plugin /
    // mununu-sva image; the ordering is deterministic + host-testable).
    #[test]
    /// mununu#504 — a lift-subprocess timeout must produce an ACTIONABLE error naming the cap,
    /// not a bare failure. Pure mapper, so this runs without spawning anything.
    fn lift_timeout_outcome_maps_a_kill_to_an_actionable_error() {
        let err = lift_timeout_outcome(None, "yosys").expect_err("timeout is an error");
        assert!(
            err.message.contains("exceeded the lift timeout"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("MUNUNU_LIFT_TIMEOUT_MS"),
            "must name the knob that changes it: {}",
            err.message
        );
        assert!(
            err.message.contains("before any property"),
            "must explain WHY this costs the whole run: {}",
            err.message
        );
    }

    #[test]
    /// The non-timeout path is passed through untouched, so bounding the call cannot change
    /// behaviour on a design that completes.
    fn lift_timeout_outcome_passes_a_completed_run_through() {
        let st = std::process::Command::new("true")
            .status()
            .expect("`true` runs");
        let (status, out, err) =
            lift_timeout_outcome(Some((st, "OUT".into(), "ERR".into())), "yosys")
                .expect("completed run is not an error");
        assert!(status.success());
        assert_eq!(out, "OUT");
        assert_eq!(err, "ERR");
    }

    #[test]
    /// mununu#504 — the ladder, pure. `0` disables (the escape-hatch convention the other
    /// budgets use); a positive value is honoured; unset or garbage falls back to the default.
    fn lift_timeout_ms_parses_the_whole_ladder() {
        use crate::adapter::parse_lift_timeout_ms;
        assert_eq!(parse_lift_timeout_ms(Some("0".into())), None, "0 disables");
        assert_eq!(
            parse_lift_timeout_ms(Some("1500".into())),
            Some(std::time::Duration::from_millis(1500))
        );
        let default = parse_lift_timeout_ms(None).expect("unset ⇒ default");
        assert_eq!(default, std::time::Duration::from_secs(15 * 60));
        assert_eq!(
            parse_lift_timeout_ms(Some("garbage".into())),
            Some(default),
            "unparseable falls back to the default rather than disabling the cap"
        );
        assert_eq!(parse_lift_timeout_ms(Some("  0  ".into())), None, "trimmed");
    }

    #[test]
    /// mununu#504 — the helper really does kill a hung child, and fast. This is what turns
    /// "does it hang?" into a deterministic sub-second test instead of a six-hour one.
    fn run_with_timeout_kills_a_hanging_child_quickly() {
        let t0 = std::time::Instant::now();
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        let outcome =
            crate::adapter::run_with_timeout(&mut cmd, None, std::time::Duration::from_millis(150))
                .expect("spawn ok");
        assert!(outcome.is_none(), "a killed child reports as a timeout");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "must return promptly after the cap, took {:?}",
            t0.elapsed()
        );
    }

    #[test]
    fn auto_frontend_attempt_order() {
        // No slang plugin → the sole attempt is read_verilog (unchanged legacy behaviour).
        assert_eq!(
            frontend_attempt_order(false, false),
            vec![SvFrontend::Verilog]
        );
        assert_eq!(
            frontend_attempt_order(false, true),
            vec![SvFrontend::Verilog]
        );
        // slang present, default preference → verilog first, slang fallback (broad default leads).
        assert_eq!(
            frontend_attempt_order(true, false),
            vec![SvFrontend::Verilog, SvFrontend::Slang]
        );
        // MUNUNU_YOSYS_FRONTEND=slang flips the preference → slang first, verilog fallback.
        assert_eq!(
            frontend_attempt_order(true, true),
            vec![SvFrontend::Slang, SvFrontend::Verilog]
        );
    }

    // P1.1 — the Auto-frontend fallback, end-to-end. `read_verilog` rejects a module-header
    // package import (`module m import p::*;`) with `unexpected TOK_IMPORT`; Auto must retry with
    // slang and lift. (Unblocks OpenTitan-class designs — the roadmap's stated P1.1 blocker.)
    // Needs the yosys-slang plugin ⇒ mununu-sva image.
    #[test]
    #[ignore = "requires the yosys-slang plugin (mununu-sva image)"]
    fn e2e_auto_frontend_falls_back_to_slang_on_module_import() {
        const IMPORT_SV: &str = "\
package p; typedef enum logic [1:0] {A=0, B=1, C=2} st_e; endpackage
module m import p::*; (input logic clk, output logic [1:0] o);
  st_e s = A;
  always_ff @(posedge clk) s <= (s == C) ? A : st_e'(s + 2'd1);
  assign o = s;
endmodule
";
        // Auto → read_verilog fails (TOK_IMPORT) → falls back to slang → lifts.
        let auto = YosysOptions {
            frontend: SvFrontend::Auto,
            ..Default::default()
        };
        let btor2 = sv_to_btor2(IMPORT_SV, &auto)
            .expect("Auto must fall back to slang and lift a module-header package import");
        assert!(!btor2.is_empty(), "the fallback lift produced empty BTOR2");
        // An EXPLICIT Verilog frontend must still FAIL on the same input — proving it is the
        // fallback (not read_verilog) that lifted it, and that an explicit request is honoured.
        let verilog = YosysOptions {
            frontend: SvFrontend::Verilog,
            ..Default::default()
        };
        assert!(
            sv_to_btor2(IMPORT_SV, &verilog).is_err(),
            "read_verilog must reject the module-header import (no silent fallback on explicit request)"
        );
    }

    // A2 — port net-type stripping (`input tri0 x` → `input x`).
    #[test]
    fn strip_port_net_types_drops_input_tri0() {
        // ANSI port list (the rtfsimpleuart case).
        assert_eq!(
            strip_port_net_types("module m(\n  input cs_i,\n  input tri0 baud8x, // mode\n);"),
            "module m(\n  input cs_i,\n  input baud8x, // mode\n);"
        );
        // Non-ANSI port declaration statement.
        assert_eq!(strip_port_net_types("input tri0 baud8x;"), "input baud8x;");
        // output / inout + the other rejected net types.
        assert_eq!(strip_port_net_types("output tri1 y;"), "output y;");
        assert_eq!(strip_port_net_types("inout wand bus;"), "inout bus;");
        // A ranged port keeps the range (the net type precedes it).
        assert_eq!(
            strip_port_net_types("input tri0 [7:0] d;"),
            "input [7:0] d;"
        );
    }

    #[test]
    fn strip_port_net_types_leaves_non_targets_untouched() {
        // `wire`/`reg`/`logic` on a port are accepted by yosys — keep them.
        assert_eq!(strip_port_net_types("input wire clk;"), "input wire clk;");
        assert_eq!(
            strip_port_net_types("output reg [3:0] q;"),
            "output reg [3:0] q;"
        );
        // An INTERNAL net-type declaration (no direction before it) is out of
        // scope — its pull/resolution semantics matter, so it is NOT stripped.
        assert_eq!(
            strip_port_net_types("tri0 internal_net;"),
            "tri0 internal_net;"
        );
        assert_eq!(
            strip_port_net_types("wor wired_or_net;"),
            "wor wired_or_net;"
        );
        // No net types at all: exact clone.
        assert_eq!(
            strip_port_net_types("input clk; output y;"),
            "input clk; output y;"
        );
    }

    #[test]
    fn strip_port_net_types_is_comment_and_string_safe() {
        // `tri0` inside a line comment / block comment / string is never rewritten.
        assert_eq!(
            strip_port_net_types("// input tri0 x\ninput clk;"),
            "// input tri0 x\ninput clk;"
        );
        assert_eq!(
            strip_port_net_types("/* input tri0 x */ input clk;"),
            "/* input tri0 x */ input clk;"
        );
        assert_eq!(
            strip_port_net_types("$display(\"input tri0 x\"); input clk;"),
            "$display(\"input tri0 x\"); input clk;"
        );
    }

    #[test]
    fn strip_port_net_types_preserves_escaped_identifier_named_tri0() {
        // `\tri0` is an escaped IDENTIFIER (a legal port NAME yosys accepts), not
        // the net-type keyword — it must survive.
        let src = "input \\tri0 ;";
        assert_eq!(strip_port_net_types(src), src);
    }

    #[test]
    fn per_module_yosys_timeout_is_an_actionable_error() {
        // A timed-out (killed) per-module yosys call must surface an ACTIONABLE
        // scope error — naming the cap, the small-fixture scope, and the
        // whole-design alternative — not hang and not a bare "failure".
        let err = yosys_run_outcome_to_result(None, "write_btor …", Duration::from_secs(60))
            .expect_err("a timeout must be an error");
        assert_eq!(err.kind, AdapterErrorKind::ParseError);
        assert!(
            err.message.contains("exceeded 60s"),
            "names the cap: {}",
            err.message
        );
        assert!(
            err.message.contains("multi-module fixtures"),
            "names the scope: {}",
            err.message
        );
        assert!(
            err.message.contains("verify-auto"),
            "points at the whole-design path: {}",
            err.message
        );
    }

    fn yosys_available() -> bool {
        Command::new("yosys")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    // ----- R-MM-3: top-module instance connectivity parsing ----------
    //
    // The embedded snapshot mirrors the real `write_json` output captured
    // from `producer_consumer_top` (yosys `hierarchy -top … ; proc ;
    // write_json`): three instances over a shared `valid` net (producer
    // drives it, buffer reads it as `push`, consumer reads it as `valid`)
    // plus broadcast `clk` / `rst`. The load-bearing assertion is that the
    // parser identifies `u_producer.valid` and `u_buffer.push` as the SAME
    // net (`valid`), which is what lets the driver make them rendezvous.
    const HIER_JSON_PRODUCER_CONSUMER: &str = r#"{
      "modules": {
        "producer_consumer_top": {
          "attributes": { "top": "00000000000000000000000000000001" },
          "ports": {
            "clk": { "direction": "input", "bits": [2] },
            "rst": { "direction": "input", "bits": [3] },
            "enable": { "direction": "input", "bits": [4] }
          },
          "cells": {
            "u_producer": {
              "type": "producer",
              "connections": {
                "clk": [2], "rst": [3], "enable": [4], "valid": [7]
              }
            },
            "u_buffer": {
              "type": "bounded_buffer",
              "connections": {
                "clk": [2], "rst": [3], "push": [7], "pop": [6], "full": [5]
              }
            },
            "u_consumer": {
              "type": "consumer",
              "connections": { "clk": [2], "rst": [3], "valid": [7] }
            }
          },
          "netnames": {
            "clk": { "bits": [2] },
            "rst": { "bits": [3] },
            "enable": { "bits": [4] },
            "full": { "bits": [5] },
            "pop_sig": { "bits": [6] },
            "valid": { "bits": [7] }
          }
        },
        "producer": { "ports": { "valid": { "direction": "output", "bits": [2] } } }
      }
    }"#;

    #[test]
    fn parse_instance_connections_resolves_shared_net() {
        let conns = parse_instance_connections(HIER_JSON_PRODUCER_CONSUMER, None);
        // Three instances, sorted by instance name.
        let names: Vec<&str> = conns.iter().map(|c| c.instance.as_str()).collect();
        assert_eq!(names, vec!["u_buffer", "u_consumer", "u_producer"]);

        let by_name = |n: &str| conns.iter().find(|c| c.instance == n).unwrap();

        let producer = by_name("u_producer");
        assert_eq!(producer.module_type, "producer");
        assert_eq!(
            producer.port_to_net.get("valid").map(|s| s.as_str()),
            Some("valid")
        );
        assert_eq!(
            producer.port_to_net.get("clk").map(|s| s.as_str()),
            Some("clk")
        );
        assert_eq!(
            producer.port_to_net.get("enable").map(|s| s.as_str()),
            Some("enable")
        );

        let buffer = by_name("u_buffer");
        assert_eq!(buffer.module_type, "bounded_buffer");
        // The crux: the buffer's `push` port and the producer's `valid`
        // port both resolve to net `valid` → they will rendezvous.
        assert_eq!(
            buffer.port_to_net.get("push").map(|s| s.as_str()),
            Some("valid")
        );
        assert_eq!(
            buffer.port_to_net.get("pop").map(|s| s.as_str()),
            Some("pop_sig")
        );
        assert_eq!(
            buffer.port_to_net.get("full").map(|s| s.as_str()),
            Some("full")
        );

        let consumer = by_name("u_consumer");
        assert_eq!(consumer.module_type, "consumer");
        assert_eq!(
            consumer.port_to_net.get("valid").map(|s| s.as_str()),
            Some("valid")
        );

        // Broadcast nets resolve identically across instances → the driver
        // synchronises clk/rst across all three.
        assert_eq!(
            producer.port_to_net.get("clk"),
            buffer.port_to_net.get("clk")
        );
        assert_eq!(
            buffer.port_to_net.get("clk"),
            consumer.port_to_net.get("clk")
        );
    }

    #[test]
    fn parse_instance_connections_handles_missing_top_and_garbage() {
        // No top flag, no explicit top → empty (best-effort).
        assert!(parse_instance_connections(r#"{"modules":{"m":{}}}"#, None).is_empty());
        // Unparseable JSON → empty, never panics.
        assert!(parse_instance_connections("not json", Some("top")).is_empty());
        // Explicit top with no cells → empty instance list.
        assert!(
            parse_instance_connections(r#"{"modules":{"top":{"netnames":{}}}}"#, Some("top"))
                .is_empty()
        );
    }

    /// R-MM-4a — `translate_sv_per_module_with_connectivity` surfaces the
    /// top-module instance connectivity alongside the per-module BTOR2,
    /// from a single discovery pass. Validates the shared-net wiring the
    /// composition driver depends on: `producer_consumer_top` has a `valid`
    /// net driven by `u_producer.valid` and read by `u_buffer.push` /
    /// `u_consumer.valid`.
    #[test]
    fn per_module_with_connectivity_surfaces_shared_net() {
        if !yosys_available() {
            eprintln!("skip: yosys not installed");
            return;
        }
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/systemverilog");
        let read = |f: &str| std::fs::read_to_string(dir.join(f));
        let (top, producer, consumer, buffer) = match (
            read("multi_producer_consumer_top.sv"),
            read("multi_producer.sv"),
            read("multi_consumer.sv"),
            read("bounded_buffer.sv"),
        ) {
            (Ok(t), Ok(p), Ok(c), Ok(b)) => (t, p, c, b),
            _ => {
                eprintln!("skip: multi-module fixtures not found");
                return;
            }
        };
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("producer_consumer_top".into()),
            per_module_btor: true,
            additional_sources: vec![
                ("multi_producer.sv".into(), producer),
                ("multi_consumer.sv".into(), consumer),
                ("bounded_buffer.sv".into(), buffer),
            ],
            ..Default::default()
        };

        let (outputs, connectivity, _directions) =
            translate_sv_per_module_with_connectivity(&top, &opts, &yopts)
                .expect("per-module + connectivity");

        // Per-module BTOR2 still produced (one AdapterOutput per type).
        let types: std::collections::BTreeSet<&str> =
            outputs.iter().map(|o| o.module_name.as_str()).collect();
        assert!(types.contains("producer"), "producer lifted");
        assert!(types.contains("consumer"), "consumer lifted");
        assert!(types.contains("bounded_buffer"), "bounded_buffer lifted");

        // Connectivity surfaced for all three instances.
        let inst = |n: &str| connectivity.iter().find(|c| c.instance == n);
        let producer_i = inst("u_producer").expect("u_producer connectivity");
        let buffer_i = inst("u_buffer").expect("u_buffer connectivity");
        let consumer_i = inst("u_consumer").expect("u_consumer connectivity");

        // The shared `valid` net: producer drives it (.valid), buffer reads
        // it as .push, consumer reads it as .valid — all resolve to net
        // `valid`, which is what lets the driver make them rendezvous.
        assert_eq!(
            producer_i.port_to_net.get("valid").map(String::as_str),
            Some("valid")
        );
        assert_eq!(
            buffer_i.port_to_net.get("push").map(String::as_str),
            Some("valid")
        );
        assert_eq!(
            consumer_i.port_to_net.get("valid").map(String::as_str),
            Some("valid")
        );
    }

    /// R-MM-4b — `AdapterOptions::surface_output_ports` makes the per-module
    /// lift surface a net-driving combinational OUTPUT as a per-state
    /// valuation. Without it, the producer's `valid` (a Moore fn of its
    /// register) is dropped entirely; with it, every state carries
    /// `valid = T/F` — the value the driver turns into rendezvous labels.
    #[test]
    fn surface_output_ports_surfaces_moore_output_valuation() {
        if !yosys_available() {
            eprintln!("skip: yosys not installed");
            return;
        }
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/systemverilog");
        let producer = match std::fs::read_to_string(dir.join("multi_producer.sv")) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("skip: multi_producer.sv not found");
                return;
            }
        };
        let yopts = YosysOptions {
            top: Some("producer".into()),
            per_module_btor: true,
            ..Default::default()
        };

        // Baseline: without surfacing, `valid` appears in NO state valuation.
        let baseline = translate_sv_per_module(&producer, &AdapterOptions::default(), &yopts)
            .expect("baseline per-module");
        let baseline_has_valid = baseline.iter().any(|o| {
            o.output
                .state_valuations
                .values()
                .flat_map(|states| states.values())
                .any(|vals| vals.contains_key("valid"))
        });
        assert!(
            !baseline_has_valid,
            "valid must be dropped without surfacing"
        );

        // With surface_output_ports=["valid"], every state carries valid=T/F.
        let opts = AdapterOptions {
            surface_output_ports: vec!["valid".to_string()],
            ..Default::default()
        };
        let surfaced = translate_sv_per_module(&producer, &opts, &yopts).expect("surfaced");
        let producer_out = surfaced
            .iter()
            .find(|o| o.module_name == "producer")
            .expect("producer module");
        let vals: Vec<&str> = producer_out
            .output
            .state_valuations
            .values()
            .flat_map(|states| states.values())
            .filter_map(|m| m.get("valid").map(String::as_str))
            .collect();
        assert!(!vals.is_empty(), "valid surfaced as a valuation");
        // valid = (state == 1): true in exactly one of the 4 register-states.
        assert!(vals.contains(&"T"), "valid=T in some state");
        assert!(vals.contains(&"F"), "valid=F in some state");
    }

    // ----- R-Y1 (§Phase 8): setundef 3-way precedence ----------------

    #[test]
    fn select_setundef_pass_defaults_to_zero() {
        assert_eq!(select_setundef_pass(false, false), "setundef -zero");
    }

    #[test]
    fn select_setundef_pass_anyconst_wins_over_zero() {
        assert_eq!(select_setundef_pass(false, true), "setundef -anyconst");
    }

    #[test]
    fn select_setundef_pass_anyseq_wins_over_anyconst() {
        // Precedence: anyseq is strictly more permissive than anyconst.
        // When both flags are set, anyseq must win so the caller gets
        // the most-bug-bearing semantics (zero false-negatives at the
        // cost of state-space).
        assert_eq!(select_setundef_pass(true, true), "setundef -anyseq");
    }

    #[test]
    fn select_setundef_pass_anyseq_wins_when_alone() {
        assert_eq!(select_setundef_pass(true, false), "setundef -anyseq");
    }

    #[test]
    fn yosys_options_default_has_setundef_anyconst_false() {
        // R-Y1 strict additivity: the new field must default to false
        // so existing fixtures' verdicts are unchanged.
        let opts = YosysOptions::default();
        assert!(!opts.setundef_anyconst);
        assert!(!opts.setundef_anyseq);
    }

    #[test]
    fn build_per_module_script_emits_anyconst_when_flagged() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/foo.sv")];
        let btor = PathBuf::from("/tmp/out.btor");
        let no_overrides: InitPolicyOverrides = Vec::new();

        let zero_script =
            build_per_module_script(&sources, "top", &btor, false, false, &no_overrides, &[]);
        assert!(zero_script.contains("setundef -zero"));
        assert!(!zero_script.contains("setundef -anyconst"));
        assert!(!zero_script.contains("setundef -anyseq"));

        let anyconst_script =
            build_per_module_script(&sources, "top", &btor, false, true, &no_overrides, &[]);
        assert!(anyconst_script.contains("setundef -anyconst"));
        assert!(!anyconst_script.contains("setundef -zero"));

        let anyseq_script =
            build_per_module_script(&sources, "top", &btor, true, false, &no_overrides, &[]);
        assert!(anyseq_script.contains("setundef -anyseq"));
        assert!(!anyseq_script.contains("setundef -anyconst"));

        // Precedence: both flags set → anyseq wins
        let both_script =
            build_per_module_script(&sources, "top", &btor, true, true, &no_overrides, &[]);
        assert!(both_script.contains("setundef -anyseq"));
        assert!(!both_script.contains("setundef -anyconst"));
    }

    // ---- R-Y2 (§Phase 8 §8.1): per-signal init policy ----

    #[test]
    fn emit_init_policy_setattrs_empty_returns_empty_string() {
        let empty: InitPolicyOverrides = Vec::new();
        assert!(emit_init_policy_setattrs(&empty).is_empty());
    }

    #[test]
    fn emit_init_policy_setattrs_skips_inherit() {
        use crate::adapter::systemverilog::annotation::InitPolicy;
        let overrides: InitPolicyOverrides = vec![("ignored".to_string(), InitPolicy::Inherit)];
        // Inherit yields no attribute → emitted string is empty.
        assert!(emit_init_policy_setattrs(&overrides).is_empty());
    }

    #[test]
    fn emit_init_policy_setattrs_anyconst_per_signal() {
        use crate::adapter::systemverilog::annotation::InitPolicy;
        let overrides: InitPolicyOverrides = vec![
            ("boot_fsm_ns".to_string(), InitPolicy::Anyconst),
            ("other_reg".to_string(), InitPolicy::Zero),
        ];
        let cmds = emit_init_policy_setattrs(&overrides);
        assert!(cmds.contains("setattr -set anyconst 1 w:boot_fsm_ns"));
        assert!(cmds.contains("setattr -set init 0 w:other_reg"));
        // Trailing semicolon-space so caller can concatenate.
        assert!(cmds.ends_with("; "));
    }

    #[test]
    fn build_per_module_script_includes_per_signal_setattrs() {
        use crate::adapter::systemverilog::annotation::InitPolicy;
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/foo.sv")];
        let btor = PathBuf::from("/tmp/out.btor");
        let overrides: InitPolicyOverrides =
            vec![("boot_fsm_ns".to_string(), InitPolicy::Anyconst)];
        // Global policy stays default (-zero); per-signal anyconst
        // applies only to boot_fsm_ns.
        let script = build_per_module_script(&sources, "top", &btor, false, false, &overrides, &[]);
        assert!(
            script.contains("setattr -set anyconst 1 w:boot_fsm_ns"),
            "per-signal setattr missing from script: {script}"
        );
        assert!(
            script.contains("setundef -zero"),
            "global -zero policy should still apply: {script}"
        );
        // Per-signal setattr comes between hierarchy and proc.
        let hier_pos = script.find("hierarchy").expect("hierarchy command present");
        let setattr_pos = script
            .find("setattr -set anyconst")
            .expect("setattr present");
        let proc_pos = script.find("; proc;").expect("proc command present");
        assert!(
            hier_pos < setattr_pos && setattr_pos < proc_pos,
            "setattr must appear between hierarchy and proc; ordering = hier@{hier_pos} setattr@{setattr_pos} proc@{proc_pos}"
        );
    }

    // ----- Control-slice cut-point tests (yosys-free) ----------------

    #[test]
    fn is_valid_net_name_accepts_identifiers_and_selects() {
        assert!(is_valid_net_name("must_refresh"));
        assert!(is_valid_net_name("_leading_underscore"));
        assert!(is_valid_net_name("top.sub.sig"));
        assert!(is_valid_net_name("cnt_q[3:0]"));
        assert!(is_valid_net_name("gen$flop"));
    }

    #[test]
    fn is_valid_net_name_rejects_injection_and_bad_starts() {
        assert!(!is_valid_net_name("")); // empty
        assert!(!is_valid_net_name("3state")); // leading digit
        assert!(!is_valid_net_name("a; write_verilog /etc/passwd")); // pass injection via `;`
        assert!(!is_valid_net_name("a b")); // whitespace
        assert!(!is_valid_net_name("a\"b")); // quote
        assert!(!is_valid_net_name("a-b")); // stray operator
    }

    #[test]
    fn emit_cutpoints_empty_and_populated() {
        assert_eq!(emit_cutpoints(&[]), "");
        assert_eq!(
            emit_cutpoints(&["must_refresh".to_string(), "precharge_done".to_string()]),
            "cutpoint w:must_refresh w:precharge_done; "
        );
        // Invalid names are dropped; a lone invalid name yields the empty (no-op) pass.
        assert_eq!(emit_cutpoints(&["bad;name".to_string()]), "");
        assert_eq!(
            emit_cutpoints(&["bad;name".to_string(), "ok_net".to_string()]),
            "cutpoint w:ok_net; "
        );
    }

    #[test]
    fn build_script_injects_cutpoint_between_write_json_and_flatten() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/foo.sv")];
        let btor = PathBuf::from("/tmp/out.btor");
        let hier = PathBuf::from("/tmp/hier.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let cuts = vec!["must_refresh".to_string(), "precharge_done".to_string()];
        let script = build_script(
            &sources,
            Some("top"),
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &cuts,
            false,
            &[],
            &[],
        );
        assert!(
            script.contains("cutpoint w:must_refresh w:precharge_done"),
            "explicit cut points missing from script: {script}"
        );
        // Ordering: the control-slice cut points come after the discovery `write_json`
        // snapshot (so state discovery sees the real nets) and before `flatten`.
        let wj = script.find("write_json").expect("write_json present");
        let cut = script
            .find("cutpoint w:must_refresh")
            .expect("explicit cutpoint present");
        let flat = script.find("flatten").expect("flatten present");
        assert!(
            wj < cut && cut < flat,
            "cutpoint must sit between write_json and flatten; wj@{wj} cut@{cut} flatten@{flat}"
        );
    }

    #[test]
    fn build_script_without_cutpoints_has_no_explicit_cutpoint_pass() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/foo.sv")];
        let btor = PathBuf::from("/tmp/out.btor");
        let hier = PathBuf::from("/tmp/hier.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let script = build_script(
            &sources,
            Some("top"),
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            false,
            &[],
            &[],
        );
        // The blackbox cut is always present; there must be no explicit `cutpoint w:` pass.
        assert!(script.contains("cutpoint -blackbox"));
        assert!(
            !script.contains("cutpoint w:"),
            "no explicit cut points expected: {script}"
        );
    }

    // A3 — the slang RTL frontend read command: ONE `read_slang` over all sources with
    // assertions ignored, no `read_verilog`, and the same frontend-agnostic passes after.
    #[test]
    fn build_script_slang_frontend_uses_read_slang() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/a.sv"), PathBuf::from("/tmp/b.sv")];
        let btor = PathBuf::from("/tmp/out.btor");
        let hier = PathBuf::from("/tmp/hier.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let script = build_script(
            &sources,
            Some("top"),
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            true, // use_slang
            &[],
            &[],
        );
        assert!(
            script.contains("read_slang --ignore-assertions"),
            "slang read command missing: {script}"
        );
        assert!(
            script.contains("--ignore-timing"),
            "read_slang must pass --ignore-timing (read_verilog-equivalent #delay handling): {script}"
        );
        assert!(
            script.contains("--top top"),
            "top not passed to read_slang: {script}"
        );
        assert!(
            script.contains("/tmp/a.sv") && script.contains("/tmp/b.sv"),
            "both sources must be read in one read_slang: {script}"
        );
        assert!(
            !script.contains("read_verilog"),
            "read_verilog must not appear on the slang path: {script}"
        );
        // Downstream is frontend-agnostic — the same lift passes run either way.
        assert!(
            script.contains("async2sync")
                && script.contains("dffunmap")
                && script.contains("write_btor"),
            "downstream passes missing: {script}"
        );
    }

    // (b) source-manifest lift — caller-supplied include dirs surface as
    // attached `-I<dir>` flags on the read command (verified empirically to be
    // the form yosys `read_verilog`/`read_slang` accept). Without them a
    // `` `include "frag.vh" `` that lives outside the staging tempdir fails.
    #[test]
    fn build_script_read_verilog_forwards_extra_include_dirs() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/top.v")];
        let btor = PathBuf::from("/tmp/out.btor");
        let hier = PathBuf::from("/tmp/hier.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let inc = vec![
            PathBuf::from("/design/include"),
            PathBuf::from("/design/rtl"),
        ];
        let script = build_script(
            &sources,
            None,
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            false,
            &inc,
            &[],
        );
        assert!(
            script.contains("read_verilog -formal -sv -I/design/include -I/design/rtl /tmp/top.v"),
            "include dirs not forwarded to read_verilog: {script}"
        );
    }

    #[test]
    fn chparam_passes_scope_and_quote_correctly() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/dut.sv")];
        let btor = PathBuf::from("/tmp/o.btor");
        let hier = PathBuf::from("/tmp/h.json");
        let overrides: InitPolicyOverrides = Vec::new();
        // bare NAME → the top module; MODULE.NAME → that module; a non-integer
        // value is quoted; integers pass through.
        let params = vec![
            ("INIT_WAIT".to_string(), "4".to_string()),
            ("u_sub.DEPTH".to_string(), "8".to_string()),
            ("MODE".to_string(), "fast".to_string()),
        ];
        let script = build_script(
            &sources,
            Some("dut"),
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            false,
            &[],
            &params,
        );
        assert!(
            script.contains("chparam -set INIT_WAIT 4 dut"),
            "bare NAME → top module: {script}"
        );
        assert!(
            script.contains("chparam -set DEPTH 8 u_sub"),
            "MODULE.NAME → that module: {script}"
        );
        assert!(
            script.contains("chparam -set MODE \"fast\" dut"),
            "non-integer value must be quoted: {script}"
        );
        // chparam runs BEFORE hierarchy (so it sizes the design before elaboration).
        let cp = script.find("chparam").expect("chparam present");
        let hi = script.find("hierarchy").expect("hierarchy present");
        assert!(cp < hi, "chparam must precede hierarchy: {script}");
    }

    #[test]
    fn slang_frontend_uses_param_g_not_chparam() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/dut.sv")];
        let btor = PathBuf::from("/tmp/o.btor");
        let hier = PathBuf::from("/tmp/h.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let params = vec![
            ("INIT_WAIT".to_string(), "4".to_string()),
            ("u_sub.DEPTH".to_string(), "8".to_string()),
        ];
        let script = build_script(
            &sources,
            Some("dut"),
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            true, // use_slang
            &[],
            &params,
        );
        // slang elaborates at read time, so it takes `-G NAME=VALUE` overrides —
        // chparam (which the read_verilog path uses) would be too late.
        assert!(script.contains("read_slang"), "{script}");
        assert!(
            script.contains("-G INIT_WAIT=4"),
            "bare NAME → -G: {script}"
        );
        assert!(
            script.contains("-G DEPTH=8"),
            "MODULE.NAME → -G on the bare name: {script}"
        );
        assert!(
            !script.contains("chparam"),
            "slang path must NOT emit chparam: {script}"
        );
    }

    #[test]
    fn chparam_bare_name_without_top_has_no_module_selection() {
        // Auto-top: a bare NAME gets no module selection — yosys applies it where
        // the parameter exists (and errors if nowhere).
        assert_eq!(
            build_chparam_passes(&[("K".to_string(), "2".to_string())], None),
            "chparam -set K 2; "
        );
        assert_eq!(build_chparam_passes(&[], Some("top")), "");
    }

    #[test]
    fn build_script_read_verilog_no_include_dirs_is_unchanged() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/top.v")];
        let btor = PathBuf::from("/tmp/out.btor");
        let hier = PathBuf::from("/tmp/hier.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let script = build_script(
            &sources,
            None,
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            false,
            &[],
            &[],
        );
        // Empty extra include dirs → no `-I` on the read command (historical form).
        assert!(
            script.contains("read_verilog -formal -sv /tmp/top.v"),
            "read command changed shape with no include dirs: {script}"
        );
        assert!(
            !script.contains(" -I"),
            "no `-I` should appear when no include dirs are supplied: {script}"
        );
    }

    // Slang frontend also forwards the caller dirs (appended after the staging
    // tempdir include the read_slang path already adds).
    #[test]
    fn build_script_read_slang_forwards_extra_include_dirs() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/stage/a.sv")];
        let btor = PathBuf::from("/tmp/out.btor");
        let hier = PathBuf::from("/tmp/hier.json");
        let overrides: InitPolicyOverrides = Vec::new();
        let inc = vec![PathBuf::from("/design/pkg")];
        let script = build_script(
            &sources,
            None,
            &btor,
            &hier,
            false,
            false,
            &overrides,
            &[],
            true,
            &inc,
            &[],
        );
        assert!(
            script.contains(
                "read_slang --ignore-assertions --ignore-timing -I/stage -I/design/pkg /stage/a.sv"
            ),
            "caller include dir not appended on the slang path: {script}"
        );
    }

    // (b) end-to-end — an include-only fragment (a mid-`always` statement, NOT a
    // standalone compilation unit) resolves via `extra_include_dirs` `-I`, so the
    // design lifts WITHOUT the fragment being passed as a compiled source (which
    // yosys would parse in isolation → `syntax error, unexpected TOK_*`). This is
    // the per-design source-manifest multi-file lift the AssertLLM2 corpus needs.
    // yosys-gated (`#[ignore]`, like the other lift e2e tests); the script-level
    // mechanism is covered by the non-ignored `build_script_*_forwards_*` tests.
    #[test]
    #[ignore = "requires yosys on PATH or MUNUNU_YOSYS_PATH"]
    fn extra_include_dirs_resolves_fragment_without_standalone_compile() {
        let tmp = TempDir::new("mununu-inc-e2e").expect("tempdir");
        let inc = tmp.path().join("inc");
        std::fs::create_dir_all(&inc).expect("mkdir inc");
        std::fs::write(inc.join("counter_body.vh"), "q <= q + 8'd1;\n").expect("write frag");
        let top = "module top(input clk, input rst, output reg [7:0] q);\n\
                   always @(posedge clk) begin\n\
                   if (rst) q <= 8'd0; else begin\n\
                   `include \"counter_body.vh\"\n\
                   end end endmodule\n";

        // WITH the include dir → the `\`include` resolves and the design lifts.
        let with_inc = YosysOptions {
            top: Some("top".to_string()),
            extra_include_dirs: vec![inc.clone()],
            skip_verific_check: true,
            ..Default::default()
        };
        let lifted = sv_to_btor2(top, &with_inc);
        assert!(
            lifted.as_ref().is_ok_and(|b| !b.is_empty()),
            "expected a clean lift with -I<incdir>, got {lifted:?}"
        );

        // WITHOUT it (and without passing the fragment as a source) → the include
        // cannot be resolved, so the lift fails cleanly rather than silently.
        let no_inc = YosysOptions {
            top: Some("top".to_string()),
            skip_verific_check: true,
            ..Default::default()
        };
        assert!(
            sv_to_btor2(top, &no_inc).is_err(),
            "expected the lift to fail when the include dir is absent"
        );
    }

    // (a) front-end selection — a non-constant `while` loop is rejected by yosys
    // `read_verilog` ("While loops are only allowed in constant functions!") but
    // accepted by the slang front-end (`read_slang`), which elaborates it. This is
    // the real corpus lift blocker the slang front-end unlocks (e.g. AssertLLM2
    // `xgate`). yosys + yosys-slang-plugin gated → runs in the `mununu-sva` image.
    #[test]
    #[ignore = "requires yosys + the yosys-slang plugin (run in the mununu-sva image)"]
    fn frontend_slang_lifts_while_loop_read_verilog_rejects() {
        let src = "module m(input [3:0] n, output reg [7:0] y);\n\
                   integer i;\n\
                   always @* begin\n\
                     y = 8'd0; i = 0;\n\
                     while (i < n) begin y = y + 8'd1; i = i + 1; end\n\
                   end\n\
                   endmodule\n";

        // read_verilog rejects the non-constant while loop.
        let verilog = YosysOptions {
            top: Some("m".to_string()),
            frontend: SvFrontend::Verilog,
            skip_verific_check: true,
            ..Default::default()
        };
        assert!(
            sv_to_btor2(src, &verilog).is_err(),
            "read_verilog should reject a non-constant while loop"
        );

        // The slang front-end elaborates it → the design lifts.
        let slang = YosysOptions {
            top: Some("m".to_string()),
            frontend: SvFrontend::Slang,
            skip_verific_check: true,
            ..Default::default()
        };
        let lifted = sv_to_btor2(src, &slang);
        assert!(
            lifted.as_ref().is_ok_and(|b| !b.is_empty()),
            "the slang front-end should lift the while loop, got {lifted:?}"
        );
    }

    // ----- Hierarchy JSON parser tests (yosys-free) ------------------

    #[test]
    fn parse_blackbox_modules_extracts_marked_modules() {
        let hier = r#"{
            "modules": {
                "top": {
                    "attributes": { "top": "00000000000000000000000000000001" },
                    "ports": {
                        "clk": { "direction": "input", "bits": [2] },
                        "data": { "direction": "output", "bits": [3] }
                    }
                },
                "ddr_phy": {
                    "attributes": {
                        "blackbox": "00000000000000000000000000000001",
                        "src": "rtl/vendor/ddr_phy.sv:12.1-30.6"
                    },
                    "ports": {
                        "clk": { "direction": "input", "bits": [4] },
                        "data_out": { "direction": "output", "bits": [5, 6, 7] }
                    }
                }
            }
        }"#;
        let bb = parse_blackbox_modules(hier);
        assert_eq!(bb.len(), 1);
        let m = &bb[0];
        assert_eq!(m.name, "ddr_phy");
        assert_eq!(m.source_file.as_deref(), Some("rtl/vendor/ddr_phy.sv"));
        assert_eq!(m.source_line, Some(12));
        // Ports sorted alphabetically (clk then data_out)
        assert_eq!(m.ports.len(), 2);
        let clk = m.ports.iter().find(|p| p.name == "clk").unwrap();
        assert_eq!(clk.direction, BoundaryDirection::Input);
        let data_out = m.ports.iter().find(|p| p.name == "data_out").unwrap();
        assert_eq!(data_out.direction, BoundaryDirection::Output);
    }

    #[test]
    fn parse_blackbox_modules_ignores_non_blackbox() {
        let hier = r#"{
            "modules": {
                "top": {
                    "attributes": {},
                    "ports": { "clk": { "direction": "input", "bits": [2] } }
                }
            }
        }"#;
        assert!(parse_blackbox_modules(hier).is_empty());
    }

    #[test]
    fn parse_blackbox_modules_handles_inout() {
        let hier = r#"{
            "modules": {
                "bidir": {
                    "attributes": { "blackbox": "00000000000000000000000000000001" },
                    "ports": { "data": { "direction": "inout", "bits": [3] } }
                }
            }
        }"#;
        let bb = parse_blackbox_modules(hier);
        assert_eq!(bb.len(), 1);
        assert_eq!(bb[0].ports[0].direction, BoundaryDirection::Inout);
    }

    #[test]
    fn parse_blackbox_modules_returns_empty_on_bad_json() {
        assert!(parse_blackbox_modules("not json").is_empty());
        assert!(parse_blackbox_modules("{}").is_empty());
        assert!(parse_blackbox_modules(r#"{"modules":"not-an-object"}"#).is_empty());
    }

    #[test]
    fn parse_top_module_port_directions_uses_explicit_top() {
        let hier = r#"{
            "modules": {
                "top": {
                    "attributes": { "top": "00000000000000000000000000000001" },
                    "ports": {
                        "clk":  { "direction": "input",  "bits": [2] },
                        "data": { "direction": "output", "bits": [3] }
                    }
                },
                "vendor_ip": {
                    "attributes": { "blackbox": "00000000000000000000000000000001" },
                    "ports": {
                        "stuff": { "direction": "input", "bits": [4] }
                    }
                }
            }
        }"#;
        let dirs = parse_top_module_port_directions(hier, Some("top"));
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs.get("clk"), Some(&BoundaryDirection::Input));
        assert_eq!(dirs.get("data"), Some(&BoundaryDirection::Output));
        assert!(
            !dirs.contains_key("stuff"),
            "should not include blackbox ports"
        );
    }

    #[test]
    fn parse_top_module_port_directions_falls_back_to_top_attr() {
        let hier = r#"{
            "modules": {
                "tower": {
                    "attributes": { "top": "1" },
                    "ports": { "go": { "direction": "input", "bits": [2] } }
                },
                "leaf": {
                    "attributes": {},
                    "ports": { "x": { "direction": "input", "bits": [3] } }
                }
            }
        }"#;
        let dirs = parse_top_module_port_directions(hier, None);
        // Picked `tower` because of attributes.top = 1
        assert_eq!(dirs.len(), 1);
        assert!(dirs.contains_key("go"));
    }

    #[test]
    fn parse_top_module_port_directions_returns_empty_for_bad_json() {
        assert!(parse_top_module_port_directions("not json", None).is_empty());
        assert!(parse_top_module_port_directions("{}", None).is_empty());
    }

    #[test]
    fn parse_yosys_src_handles_typical_format() {
        assert_eq!(
            parse_yosys_src("foo.sv:10.1-25.6"),
            (Some("foo.sv".to_string()), Some(10))
        );
        assert_eq!(
            parse_yosys_src("rtl/vendor/ddr.sv:42"),
            (Some("rtl/vendor/ddr.sv".to_string()), Some(42))
        );
        // Files without a line number — should still return the path.
        assert_eq!(
            parse_yosys_src("standalone.sv"),
            (Some("standalone.sv".to_string()), None)
        );
    }

    #[test]
    fn parse_blackbox_modules_deterministic_order() {
        // Two black-box modules — output should be sorted by name.
        let hier = r#"{
            "modules": {
                "zzz_module": {
                    "attributes": { "blackbox": "1" },
                    "ports": {}
                },
                "aaa_module": {
                    "attributes": { "blackbox": "1" },
                    "ports": {}
                }
            }
        }"#;
        let bb = parse_blackbox_modules(hier);
        assert_eq!(bb.len(), 2);
        assert_eq!(bb[0].name, "aaa_module");
        assert_eq!(bb[1].name, "zzz_module");
    }

    // ----- Yosys subprocess integration tests (require yosys binary) -

    #[test]
    fn locate_yosys_finds_binary_or_errors() {
        let res = locate_yosys();
        if yosys_available() {
            assert!(res.is_ok());
        } else {
            assert!(res.is_err());
        }
    }

    #[test]
    fn detect_returns_false_for_sv() {
        // YosysAdapter::detect intentionally returns false; routing happens
        // explicitly via the CLI/API frontend selector.
        assert!(!YosysAdapter::detect("module foo; endmodule"));
    }

    #[test]
    fn elaborates_minimal_counter_to_btor2() {
        if !yosys_available() {
            eprintln!("skipping: yosys not on PATH");
            return;
        }
        // 2-bit free-running counter — exercises always_ff, parameters, and
        // arithmetic (which the hand-rolled SV adapter would not handle).
        let sv = r#"
module counter (input wire clk, input wire rst);
  reg [1:0] cnt;
  always @(posedge clk) begin
    if (rst) cnt <= 2'b00;
    else     cnt <= cnt + 1;
  end
endmodule
"#;
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("counter".into()),
            ..Default::default()
        };
        let out = translate_sv(sv, &opts, &yopts).expect("yosys translate");
        // 2-bit counter → 4 states; the bit-blaster also folds in the rst
        // input as a label dimension, but the state count itself is 4.
        assert!(
            out.source_info.state_count >= 4,
            "got {}",
            out.source_info.state_count
        );
    }

    /// End-to-end Phase 1 demo: round-trips `examples/btor2/safety_demo.sv`
    /// through Yosys and verifies the SVA assertion lands as a `safety_bad_*`
    /// μ-calculus property in the resulting CTXDSL.
    ///
    /// This is the canonical Phase 1 acceptance test — if it fails, either
    /// the Yosys script or the BTOR2 reader regressed. The example file is
    /// intentionally checked in so the test exercises a real on-disk
    /// artifact, not an inline string.
    #[test]
    fn phase1_demo_safety_assertion_surfaces_as_property() {
        if !yosys_available() {
            eprintln!("skipping: yosys not on PATH");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/btor2/safety_demo.sv");
        let sv = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("skipping: example file not found at {}", path.display());
                return;
            }
        };
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("safety_demo".into()),
            ..Default::default()
        };
        let out = translate_sv(&sv, &opts, &yopts).expect("yosys translate");
        assert!(
            out.source_info.property_count >= 1,
            "expected at least one safety property"
        );
        assert!(
            out.ctxdsl.contains("safety_bad_"),
            "expected a `safety_bad_*` formula in the emitted CTXDSL"
        );
    }

    #[test]
    fn yosys_emits_blackbox_sidecars_for_marked_submodules() {
        if !yosys_available() {
            eprintln!("skip: yosys not installed");
            return;
        }
        // A trivial design: top instantiates a (* blackbox *) submodule.
        // The driver should auto-emit BlackBoxInterface + GapMarkerReport
        // sidecars for `vendor_ip` and attach them to AdapterOutput.
        let sv = r#"
            (* blackbox *)
            module vendor_ip(
                input clk,
                input start,
                output ready,
                output [7:0] data_out
            );
            endmodule

            module top(input clk, input start, output ready);
                wire [7:0] _data_out;
                vendor_ip ip(.clk(clk), .start(start), .ready(ready),
                             .data_out(_data_out));
            endmodule
        "#;
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("top".into()),
            ..Default::default()
        };
        let out = translate_sv(sv, &opts, &yopts).expect("yosys translate");

        let iface_sidecars: Vec<_> = out
            .sidecars
            .iter()
            .filter(|s| s.origin == crate::adapter::SidecarOrigin::BlackBoxInterface)
            .collect();
        assert_eq!(
            iface_sidecars.len(),
            1,
            "expected exactly one BlackBoxInterface sidecar, got {} (sidecars: {:?})",
            iface_sidecars.len(),
            out.sidecars.iter().map(|s| &s.filename).collect::<Vec<_>>()
        );
        assert_eq!(iface_sidecars[0].filename, "vendor_ip.interface.json");
        assert!(iface_sidecars[0].content.contains("vendor_ip"));
        assert!(
            iface_sidecars[0]
                .content
                .contains("\"direction\": \"Output\"")
        );
        assert!(
            iface_sidecars[0]
                .content
                .contains("\"direction\": \"Input\"")
        );

        let gap_sidecars: Vec<_> = out
            .sidecars
            .iter()
            .filter(|s| s.origin == crate::adapter::SidecarOrigin::BlackBoxGapReport)
            .collect();
        assert_eq!(gap_sidecars.len(), 1);
        assert_eq!(gap_sidecars[0].filename, "vendor_ip.gap_report.json");
        assert!(gap_sidecars[0].content.contains("output_sequencing"));
    }

    // ----- sv2v preprocessor integration -----------------------------

    /// Probe whether `sv2v` is on PATH. Mirrors `yosys_available()`.
    fn sv2v_available() -> bool {
        Command::new("sv2v")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn sv2v_preprocesses_module_header_import_then_yosys_succeeds() {
        if !yosys_available() || !sv2v_available() {
            eprintln!("skipping: yosys or sv2v not on PATH");
            return;
        }
        // SV2009/2012 module-header `import pkg::*;` — Yosys 0.59's built-in
        // parser does NOT accept this. sv2v rewrites it to Verilog-2005
        // that Yosys handles. This is the construct that blocked the
        // Caliptra-RTL #150 retry in proof-by-fire Finding 1.
        let sv = r#"
package my_pkg;
    typedef enum logic [1:0] {
        S_IDLE = 2'b00,
        S_RUN  = 2'b01,
        S_DONE = 2'b10
    } state_e;
endpackage

module dut
    import my_pkg::*;
    (input wire clk, input wire start, output state_e ps);
    state_e ns;
    always @(posedge clk) ps <= ns;
    always_comb begin
        ns = ps;
        case (ps)
            S_IDLE: if (start) ns = S_RUN;
            S_RUN:  ns = S_DONE;
            S_DONE: ns = S_IDLE;
            default: ns = S_IDLE;
        endcase
    end
endmodule
"#;
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("dut".into()),
            use_sv2v: true,
            ..Default::default()
        };
        let out = translate_sv(sv, &opts, &yopts).expect("sv2v + yosys translate");
        assert!(
            out.source_info.state_count >= 3,
            "expected ≥3 reachable states for the 3-state FSM, got {}",
            out.source_info.state_count
        );
    }

    #[test]
    fn sv2v_multi_file_cross_package_imports() {
        if !yosys_available() || !sv2v_available() {
            eprintln!("skipping: yosys or sv2v not on PATH");
            return;
        }
        // Cross-file: the module in primary.sv imports a package
        // declared in additional.sv. sv2v's documented multi-file mode
        // resolves the import in one pass before Yosys sees the
        // combined output.
        let primary = r#"
module top
    import shared_pkg::*;
    (input wire clk, output logic flag);
    state_e ps;
    always @(posedge clk) ps <= S_ON;
    assign flag = (ps == S_ON);
endmodule
"#;
        let additional = r#"
package shared_pkg;
    typedef enum logic { S_OFF = 1'b0, S_ON = 1'b1 } state_e;
endpackage
"#;
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("top".into()),
            additional_sources: vec![("shared_pkg.sv".into(), additional.into())],
            use_sv2v: true,
            ..Default::default()
        };
        let out = translate_sv(primary, &opts, &yopts).expect("sv2v multi-file translate");
        // Reachability: at least 1 state. The actual count depends on
        // Yosys's flatten + chformal; we assert non-empty.
        assert!(
            out.source_info.state_count >= 1,
            "got state_count = {}",
            out.source_info.state_count
        );
    }

    #[test]
    fn sv2v_include_resolves_staged_additional_source_header() {
        if !yosys_available() || !sv2v_available() {
            eprintln!("skipping: yosys or sv2v not on PATH");
            return;
        }
        // An `\`include`d header supplied as an `additional_sources` entry
        // (staged into the per-call tempdir) must resolve — even on the
        // verify / multi-file path, which never sets
        // `primary_source_path`. Real OpenTitan RTL does
        // `\`include "prim_assert.sv"`; before the staging tempdir was
        // added to sv2v's `-I` search path the include failed with
        // "Could not find file", because sv2v does not search the
        // including file's own directory by default. Regression for the
        // R46-6 sysrst_ctrl K=5 fixture.
        let primary = r#"
`include "defs.svh"
module top (input wire clk, input wire rst_ni, output logic flag);
    logic q;
    always_ff @(posedge clk or negedge rst_ni)
        if (!rst_ni) q <= 1'b0; else q <= `MK_NEXT(q);
    assign flag = q;
endmodule
"#;
        let header = "`define MK_NEXT(x) (~(x))\n";
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("top".into()),
            // No `primary_source_path` (the verify-path shape). The header
            // resolves only because the staging tempdir is on sv2v's `-I`.
            additional_sources: vec![("defs.svh".into(), header.into())],
            use_sv2v: true,
            ..Default::default()
        };
        let out = translate_sv(primary, &opts, &yopts)
            .expect("`include of a staged additional-source header resolves");
        assert!(
            out.source_info.state_count >= 1,
            "got state_count = {}",
            out.source_info.state_count
        );
    }

    #[test]
    fn sv_discover_state_cells_reports_named_registers_and_widths() {
        // C1.1 (finding E1): discovery returns the design's named state
        // cells + widths WITHOUT bit-blasting, so it works even when the
        // raw design would bust MAX_STATE_BITS. Plain Verilog → no sv2v
        // needed (only Yosys).
        if !yosys_available() {
            eprintln!("skipping: yosys not on PATH");
            return;
        }
        let dut = r#"
module dut(input wire clk, input wire rst_ni, input wire en, output wire [7:0] o);
  reg [7:0] wide;
  reg flag;
  always @(posedge clk or negedge rst_ni)
    if (!rst_ni) begin wide <= 8'd0; flag <= 1'b0; end
    else begin if (en) wide <= wide + 8'd1; flag <= en; end
  assign o = wide;
endmodule
"#;
        let yopts = YosysOptions {
            top: Some("dut".into()),
            use_sv2v: false,
            ..Default::default()
        };
        let cells = sv_discover_state_cells(dut, &yopts).expect("discover state cells");
        let wide = cells
            .iter()
            .find(|c| c.name == "wide")
            .expect("the 8-bit `wide` register is discovered by name");
        assert_eq!(wide.width, 8, "wide register width");
        assert!(
            cells.iter().any(|c| c.name == "flag" && c.width == 1),
            "the 1-bit `flag` register is discovered too; got {cells:?}"
        );
    }

    #[test]
    fn sv2v_missing_tool_errors_cleanly() {
        // Point MUNUNU_SV2V_PATH at a definitely-missing file to make
        // locate_sv2v's first branch (env-var lookup) take a path that
        // exists in code but yields a binary that won't run. The driver
        // surfaces the failure during the sv2v subprocess spawn.
        if !yosys_available() {
            eprintln!("skipping: yosys not on PATH");
            return;
        }
        // Save/restore the env var so we don't pollute other tests.
        let prev = std::env::var("MUNUNU_SV2V_PATH").ok();
        // SAFETY: tests in this binary run on one thread by default for
        // YosysOptions-mutating cases; if parallelism is enabled this
        // test should be ignored. Per cargo test default we're fine.
        unsafe {
            std::env::set_var("MUNUNU_SV2V_PATH", "/definitely/not/a/real/sv2v/binary");
        }
        let sv = "module empty; endmodule\n";
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("empty".into()),
            use_sv2v: true,
            ..Default::default()
        };
        let res = translate_sv(sv, &opts, &yopts);
        // Restore env first so panic in assert below doesn't leak it.
        unsafe {
            match prev {
                Some(p) => std::env::set_var("MUNUNU_SV2V_PATH", p),
                None => std::env::remove_var("MUNUNU_SV2V_PATH"),
            }
        }
        let err = res.expect_err("expected sv2v spawn failure when binary path is bogus");
        assert!(
            err.message.contains("sv2v"),
            "error should name sv2v; got: {}",
            err.message
        );
    }

    #[test]
    fn strip_sv2v_mangle_demangles_param_suffix() {
        // sv2v's `_<HEX>_<HEX>` parameter-specialisation suffix is stripped.
        assert_eq!(
            strip_sv2v_mangle("prim_sparse_fsm_flop_B4CDB_707CE"),
            "prim_sparse_fsm_flop"
        );
        // A plain module name is untouched.
        assert_eq!(
            strip_sv2v_mangle("prim_sparse_fsm_flop"),
            "prim_sparse_fsm_flop"
        );
        // A name with only one trailing hex group is untouched (not the sv2v shape).
        assert_eq!(strip_sv2v_mangle("foo_DEAD"), "foo_DEAD");
    }

    #[test]
    fn parse_undefined_module_cells_finds_dangling_instance() {
        // A module that instantiates an undefined cell type (the csrng
        // prim_sparse_fsm_flop shape) — yosys leaves it as a dangling cell, not
        // a blackbox module, so it must be found via the cell scan.
        let hier = r#"{
          "modules": {
            "top": {
              "cells": {
                "u_state_regs": { "type": "prim_sparse_fsm_flop_B4CDB_707CE" },
                "an_and": { "type": "$and" },
                "u_sub": { "type": "defined_sub" }
              }
            },
            "defined_sub": { "cells": {} }
          }
        }"#;
        let found = parse_undefined_module_cells(hier);
        assert_eq!(
            found,
            vec!["prim_sparse_fsm_flop".to_string()],
            "only the undefined, non-builtin, de-mangled cell type is reported"
        );
    }

    #[test]
    fn parse_undefined_module_cells_empty_for_self_contained() {
        let hier = r#"{"modules":{"top":{"cells":{"a":{"type":"$dff"},"b":{"type":"sub"}}},"sub":{"cells":{}}}}"#;
        assert!(parse_undefined_module_cells(hier).is_empty());
    }
}
