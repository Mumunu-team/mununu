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
//!                       ▼  yosys -p "read_slang --ignore-assertions ...; hierarchy -top X; ...; chformal -lower; write_btor ..."
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
//!   would taint the binary. The driver passes `-p` and, only when
//!   `MUNUNU_YOSYS_SLANG_PLUGIN` names one, `-m <plugin>`. The front end is
//!   yosys-slang's `read_slang` (N1, 2026-11-04) — built into yosys ≥ 0.67 and the
//!   pinned `mununu-sva` image; `read_verilog` after sv2v survives only as the
//!   dev-only differential oracle of `test_support` builds ([`SvFrontend::Verilog`]).
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

/// N0.2 — the content-addressed cache of the lift's raw tool outputs (`MUNUNU_LIFT_CACHE_DIR`).
pub mod lift_cache;
/// R-MM (KMTS multi-module composition) — netlist-driven driver.
pub mod multi_module;
pub mod netlist;
pub mod source_manifest;

use crate::adapter::{
    AdapterError, AdapterErrorKind, AdapterOptions, AdapterOutput, FormatAdapter, SourceFormat,
    SourceInfo,
};
#[cfg(feature = "test_support")]
use lift_cache::Sv2vManifest;
use lift_cache::{
    AttemptManifest, CachedLift, LIFT_CACHE_FORMAT, LiftCache, LiftCacheKey, LiftCacheOutcome,
    LiftManifest, ToolIdentity, collect_include_files, file_stamp, frontend_from_tag, frontend_tag,
    key_include_directives, resolve_binary,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
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

/// The RTL front end of the SV → BTOR2 lift. The product has one: yosys-slang's `read_slang`
/// (N1, 2026-11-04) — a full SystemVerilog front end, built into yosys ≥ 0.67 and the pinned
/// `mununu-sva` image (`MUNUNU_YOSYS_SLANG_PLUGIN` names a plugin to load on a yosys without
/// it). `Verilog` — yosys `read_verilog` after sv2v — is the dev-only differential oracle of
/// `test_support` builds (two lifts of one source, the verdicts compared); the CLI and the API
/// never produce it, and a product build refuses it with a reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SvFrontend {
    /// yosys-slang's `read_slang`: the product front end.
    #[default]
    Slang,
    /// `read_verilog` after sv2v: the dev-only oracle (`test_support`); refused elsewhere.
    #[doc(hidden)]
    Verilog,
}

impl SvFrontend {
    /// Human-readable lift label for `ModelDiagnostics::frontend`.
    pub fn lift_label(self) -> &'static str {
        match self {
            SvFrontend::Slang => "yosys-slang (read_slang)",
            SvFrontend::Verilog => "read_verilog + sv2v",
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
    /// front-end as `-I<dir>` flags (`read_slang`). This lets a caller resolve
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
    /// Module-parameter overrides applied BEFORE elaboration sizes the design — slang
    /// `-G <name>=<value>` at read time (slang elaborates then). Each entry is `(lhs, value)`
    /// where `lhs` is a bare parameter name `NAME` (applied to the top module) or a scoped
    /// `MODULE.NAME` (applied by its bare name — `-G` is top-scoped). This lets a caller shrink
    /// a parameterised timing interval so the design's counters get smaller — a 20000-cycle
    /// power-up wait sizing a 15-bit counter becomes a 10-bit counter at `INIT_WAIT=4` —
    /// without a wrapper module (which would rename the SVA atoms and break binding).
    ///
    /// SOUNDNESS / scope: the verdict is scoped to the overridden parameters
    /// (echoed in the transcript + JSON report). A parameter the design does not
    /// carry is an ERROR surfaced from the lift — never a silent drop (contrast
    /// `--pin`, mununu#459); the name is validated upstream against the slang AST
    /// (`read_slang` itself would ignore an unknown `-G`).
    ///
    /// **Default**: empty. Populated from the CLI `--param NAME=VALUE` flag / the
    /// API `params` field. The dev-only `read_verilog` oracle applies them as
    /// `chparam` passes before `hierarchy`.
    pub params: Vec<(String, String)>,
    /// Optional original path of the primary SV source. Used only to
    /// rewrite the `source_file` field of any auto-emitted black-box
    /// sidecar (Document B task B3) — yosys sees the SV through a
    /// per-call tempdir, so without this hint the sidecars would point
    /// at `/tmp/.../work.sv`. The user expects the path of the file
    /// they actually invoked mununu on.
    pub primary_source_path: Option<String>,
    /// The RTL front end: [`SvFrontend::Slang`] (the default and the only product front
    /// end); [`SvFrontend::Verilog`] is the dev-only oracle of `test_support` builds.
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
    /// When `true`, a design whose flops sit in more than one clock domain (distinct clock nets,
    /// or both edges of one net) is lifted anyway, every domain stepping on the one implicit
    /// clock of the BTOR2 model. **SOUNDNESS — an UNDER-approximation of the interleavings:**
    /// the domains only ever tick together, so a crossing defect that needs one domain to tick
    /// while the other does not (a lost pulse, an incoherent multi-bit bus) is invisible and a
    /// HOLDS does not transfer. Default `false`: such a design is refused at lift time
    /// ([`clock_domains`]). verify-auto reports the merged domains as a soundness caveat.
    pub merge_clock_domains: bool,
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
    /// The front end that produced this BTOR2 — `ModelDiagnostics::frontend`.
    frontend_used: SvFrontend,
    /// `Some(reason)` when the first yosys run died in the BTOR backend with a
    /// topological loop and the script succeeded on a second run with `opt_clean`
    /// before `write_btor` (see [`backend_loop_retry_script`]).
    backend_retry: Option<String>,
    /// The clock domains of the flattened design ([`clock_domains`]); more than one only when
    /// [`YosysOptions::merge_clock_domains`] let the lift through.
    clock_domains: Vec<ClockDomain>,
    /// The flattened, `opt_clean`ed netlist the clock-domain scan read (yosys `write_json`).
    flat_netlist_json: String,
    /// What the lift cache did for this lift (N0.2): `Off`, `Hit`, `Miss` or `Bypassed(why)`.
    cache: LiftCacheOutcome,
}

/// Run the flattened Yosys script and return the design's BTOR2 text plus the
/// hierarchy snapshot. Shared by [`translate_sv`] (which bit-blasts the BTOR2) and
/// [`sv_discover_state_cells`] (which only reads the BTOR2's named state cells), so
/// the staging + Yosys-invocation logic lives in one place.
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

/// The SV → flattened-BTOR2 lift: yosys-slang's `read_slang` (N1, 2026-11-04 — before it, an
/// `Auto` order tried `read_verilog`(+sv2v) first and fell back to slang on error, so every
/// OpenTitan lift ran yosys twice and the netlist came from sv2v's rewritten text while the
/// properties came from slang's AST of the original). One grammar, one lift, no fallback.
///
/// N0.2 — under `MUNUNU_LIFT_CACHE_DIR` the lift is looked up before a tool runs and stored
/// after; see [`run_sv_flatten_btor2_with`].
fn run_sv_flatten_btor2(
    content: &str,
    yopts: &YosysOptions,
) -> Result<SvFlattenArtifacts, AdapterError> {
    run_sv_flatten_btor2_with(content, yopts, LiftCache::from_env().as_ref())
}

/// [`run_sv_flatten_btor2`] against an explicit lift cache (`None` = off) — N0.2. A hit returns
/// the stored raw tool outputs with mununu's post-processing (clock domains, the multi-clock
/// refusal) re-run on them; a miss runs the tools and stores the result; a cache that cannot be
/// used for this lift is bypassed, with the reason on the artifacts — never a failure.
fn run_sv_flatten_btor2_with(
    content: &str,
    yopts: &YosysOptions,
    cache: Option<&LiftCache>,
) -> Result<SvFlattenArtifacts, AdapterError> {
    let key: Result<Option<LiftCacheKey>, String> = match cache {
        None => Ok(None),
        Some(c) => match lift_cache_key(content, yopts) {
            Ok(k) => Ok(Some(k)),
            Err(reason) => {
                tracing::warn!(
                    target: "mununu::lift_cache",
                    dir = %c.dir().display(),
                    "lift cache bypassed: {reason}"
                );
                Err(reason)
            }
        },
    };
    if let (Some(c), Ok(Some(k))) = (cache, &key)
        && let Some(hit) = c.lookup(k)
    {
        tracing::info!(target: "mununu::lift_cache", index = k.index(), "lift cache hit");
        return artifacts_from_cached(hit, yopts);
    }
    let mut artifacts = run_sv_flatten_btor2_concrete(content, yopts)?;
    artifacts.cache = match &key {
        Ok(None) => LiftCacheOutcome::Off,
        Ok(Some(_)) => LiftCacheOutcome::Miss,
        Err(reason) => LiftCacheOutcome::Bypassed(reason.clone()),
    };
    if let (Some(c), Ok(Some(k))) = (cache, &key) {
        match c.store(k, &cached_lift_of(&artifacts)) {
            Ok(()) => tracing::info!(
                target: "mununu::lift_cache",
                index = k.index(),
                "lift cache miss: the lift is stored"
            ),
            Err(e) => tracing::warn!(
                target: "mununu::lift_cache",
                index = k.index(),
                "lift cache miss: the lift could not be stored ({e})"
            ),
        }
    }
    Ok(artifacts)
}

/// The cache key of this lift: the manifest of what it would tell its tools (N0.2). `Err` names
/// why none can be built (no yosys, an include tree over the walk cap, …) — a bypass.
fn lift_cache_key(content: &str, yopts: &YosysOptions) -> Result<LiftCacheKey, String> {
    let yosys = locate_yosys().map_err(|e| e.message)?;
    let banner = yosys_banner(&yosys).map_err(|e| e.message)?;
    // The binary's size and mtime beside its banner: a yosys rebuilt at the same version
    // banner is another tool (mununu#956).
    let bin = resolve_binary(&yosys)
        .ok_or_else(|| format!("cannot resolve the yosys binary `{}`", yosys.display()))?;
    let manifest = lift_manifest(
        content,
        yopts,
        ToolIdentity {
            path: yosys.display().to_string(),
            identity: format!("{banner}\n{}", file_stamp(&bin)?),
        },
    )?;
    LiftCacheKey::new(&manifest)
}

/// What this lift would tell its tools, planned by [`plan_lift`] against a placeholder staging
/// directory — the same planner the run executes, so the key cannot drift from the run.
/// `yosys` is the located binary's identity.
fn lift_manifest(
    content: &str,
    yopts: &YosysOptions,
    yosys: ToolIdentity,
) -> Result<LiftManifest, String> {
    let dir = Path::new("/mununu-lift-cache/staging");
    let plan = plan_lift(content, yopts, dir).map_err(|e| e.message)?;
    let staged: Vec<(String, String)> = plan
        .staged
        .iter()
        .map(|(p, text)| {
            let rel = p.strip_prefix(dir).unwrap_or(p);
            (rel.to_string_lossy().into_owned(), text.clone())
        })
        .collect();
    // The directories a tool may read `include`d files from: the script's `-I` dirs (and the
    // oracle's sv2v dirs).
    #[allow(unused_mut)]
    let mut include_dirs: Vec<PathBuf> = yopts.extra_include_dirs.clone();
    #[cfg(feature = "test_support")]
    let sv2v = match &plan.sv2v {
        Some(s) => {
            include_dirs.extend(
                s.include_dirs
                    .iter()
                    .filter(|d| d.as_path() != dir)
                    .cloned(),
            );
            Some(Sv2vManifest {
                tool: ToolIdentity {
                    path: s.bin.display().to_string(),
                    identity: sv2v_version(&s.bin).map_err(|e| e.message)?,
                },
                argv: sv2v_args(&s.sources, &s.include_dirs),
            })
        }
        None => None,
    };
    #[cfg(not(feature = "test_support"))]
    let sv2v = None;
    let plugin = plan
        .plugin
        .as_deref()
        .map(|arg| ToolIdentity::of_plugin(arg, Path::new(&yosys.path)))
        .transpose()?;
    Ok(LiftManifest {
        format: LIFT_CACHE_FORMAT,
        crate_version: env!("CARGO_PKG_VERSION"),
        yosys,
        attempt: AttemptManifest {
            frontend: frontend_tag(plan.frontend_used),
            script: plan.script,
            cwd_in_staging_dir: plan.yosys_cwd.is_some(),
            plugin,
            sv2v,
        },
        include_files: {
            let mut files = collect_include_files(&include_dirs)?;
            let walked: Vec<String> = files
                .iter()
                .filter_map(|(_, c)| c.strip_prefix("t:").map(str::to_string))
                .collect();
            let texts: Vec<&str> = staged
                .iter()
                .map(|(_, t)| t.as_str())
                .chain(walked.iter().map(String::as_str))
                .collect();
            key_include_directives(&texts, &mut files)?;
            files
        },
        staged,
    })
}

/// The artifacts of a cache hit: the stored raw outputs, with everything mununu derives from
/// them computed afresh — so a change to that code applies to hits as to misses.
fn artifacts_from_cached(
    hit: CachedLift,
    yopts: &YosysOptions,
) -> Result<SvFlattenArtifacts, AdapterError> {
    let frontend_used = frontend_from_tag(&hit.frontend_used).ok_or_else(|| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: lift cache entry names an unknown front end `{}`",
            hit.frontend_used
        ),
        location: None,
    })?;
    let clock_domains = clock_domains(&hit.flat_netlist_json);
    refuse_merged_clock_domains(&clock_domains, yopts)?;
    Ok(SvFlattenArtifacts {
        btor2: hit.btor2,
        hier_json: hit.hier_json,
        staged_primary: hit.staged_primary,
        frontend_used,
        backend_retry: hit.backend_retry,
        clock_domains,
        flat_netlist_json: hit.flat_netlist_json,
        cache: LiftCacheOutcome::Hit,
    })
}

/// The raw outputs and process facts of a successful lift, as the cache stores them.
fn cached_lift_of(a: &SvFlattenArtifacts) -> CachedLift {
    CachedLift {
        btor2: a.btor2.clone(),
        hier_json: a.hier_json.clone(),
        flat_netlist_json: a.flat_netlist_json.clone(),
        staged_primary: a.staged_primary.clone(),
        frontend_used: frontend_tag(a.frontend_used).to_string(),
        backend_retry: a.backend_retry.clone(),
    }
}

/// Everything one lift will do, decided before any tool runs: the files to stage, where
/// yosys runs and with which plugin, the exact script (and, for the dev-only oracle, the sv2v
/// invocation). [`run_sv_flatten_btor2_concrete`] executes it against a real staging directory; the
/// lift cache renders it against a placeholder one (N0.2, [`lift_manifest`]), so the cache key
/// is exactly what runs and cannot drift from it.
struct LiftPlan {
    /// The staged primary, `<dir>/work.sv`.
    primary: PathBuf,
    /// `(path under the staging dir, content)` — the primary first, each already through
    /// `strip_port_net_types`.
    staged: Vec<(PathBuf, String)>,
    /// The oracle's sv2v pass (`test_support` only; the product plans none).
    #[cfg(feature = "test_support")]
    sv2v: Option<Sv2vPlan>,
    /// yosys runs inside the staging directory when it reads sv2v's output by its bare name
    /// (mununu#806) — the names it mints depend on it. `None` on the product lift.
    yosys_cwd: Option<PathBuf>,
    /// The `-m` argument loading a slang plugin, when `MUNUNU_YOSYS_SLANG_PLUGIN` names one.
    plugin: Option<String>,
    script: String,
    frontend_used: SvFrontend,
    btor_path: PathBuf,
    hier_json_path: PathBuf,
}

/// The sv2v pass of a [`LiftPlan`]: binary, inputs, include dirs, output file (the oracle).
#[cfg(feature = "test_support")]
struct Sv2vPlan {
    bin: PathBuf,
    sources: Vec<PathBuf>,
    include_dirs: Vec<PathBuf>,
    out: PathBuf,
}

/// Plan one concrete lift of `content` staged under `dir` (which need not exist: the lift
/// cache plans against a placeholder). Touches no file; locates the tools it would run.
fn plan_lift(content: &str, yopts: &YosysOptions, dir: &Path) -> Result<LiftPlan, AdapterError> {
    let primary = dir.join("work.sv");
    // A2 — strip yosys/slang-rejected net-type qualifiers on port headers
    // (`input tri0 x`) so the design parses; sound (a port net type is invisible
    // to isolated single-module verification). No-op on sources without them.
    let mut staged = vec![(primary.clone(), strip_port_net_types(content))];
    for (name, src) in &yopts.additional_sources {
        staged.push((dir.join(name), strip_port_net_types(src)));
    }
    let sources: Vec<PathBuf> = staged.iter().map(|(p, _)| p.clone()).collect();

    // The front end (N1): yosys-slang's `read_slang`, built into yosys ≥ 0.67 and the pinned
    // image; `MUNUNU_YOSYS_SLANG_PLUGIN` names a plugin to load on a yosys without it. The
    // `read_verilog` + sv2v path is the dev-only oracle of `test_support` builds — a product
    // build refuses it here, with the reason, never with a silent swap.
    let slang_plugin = slang_plugin_arg();
    #[cfg(not(feature = "test_support"))]
    if yopts.frontend == SvFrontend::Verilog {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: "adapter/yosys: the read_verilog + sv2v lift is the dev-only differential \
                      oracle of `test_support` builds; the product lifts with yosys-slang"
                .into(),
            location: None,
        });
    }
    let frontend_used = yopts.frontend;
    #[cfg(feature = "test_support")]
    let (sv2v, yosys_cwd, sources) = plan_oracle_sv2v(sources, yopts, dir)?;
    #[cfg(not(feature = "test_support"))]
    let yosys_cwd: Option<PathBuf> = None;

    let btor_path = dir.join("design.btor");
    let hier_json_path = dir.join("hier.json");
    // With yosys's cwd moved to the staging directory, a RELATIVE caller include dir would no
    // longer resolve: hand the script absolute ones (mununu#806).
    let script_include_dirs: Vec<PathBuf> = if yosys_cwd.is_some() {
        yopts
            .extra_include_dirs
            .iter()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
            .collect()
    } else {
        yopts.extra_include_dirs.clone()
    };
    let slang_read = || {
        (
            vec![read_slang_cmd(
                &sources,
                yopts.top.as_deref(),
                &yopts.params,
                &script_include_dirs,
            )],
            String::new(),
        )
    };
    #[cfg(feature = "test_support")]
    let (read_cmds, chparam) = if frontend_used == SvFrontend::Verilog {
        (
            oracle_read_cmds(&sources, &script_include_dirs),
            build_chparam_passes(&yopts.params, yopts.top.as_deref()),
        )
    } else {
        slang_read()
    };
    #[cfg(not(feature = "test_support"))]
    let (read_cmds, chparam) = slang_read();
    let script = build_script(
        &read_cmds,
        &chparam,
        yopts.top.as_deref(),
        &btor_path,
        &hier_json_path,
        yopts.setundef_anyseq,
        yopts.setundef_anyconst,
        &yopts.init_policy_overrides,
        &yopts.cutpoint_signals,
    );
    Ok(LiftPlan {
        primary,
        staged,
        #[cfg(feature = "test_support")]
        sv2v,
        yosys_cwd,
        plugin: slang_plugin,
        script,
        frontend_used,
        btor_path,
        hier_json_path,
    })
}

/// The oracle's sv2v pass, planned (`test_support`): for [`SvFrontend::Verilog`], sv2v combines
/// the staged sources into `preprocessed.sv`, which yosys reads by its bare name from inside
/// the staging directory (mununu#806); for the product front end nothing changes.
#[cfg(feature = "test_support")]
#[allow(clippy::type_complexity)]
fn plan_oracle_sv2v(
    sources: Vec<PathBuf>,
    yopts: &YosysOptions,
    dir: &Path,
) -> Result<(Option<Sv2vPlan>, Option<PathBuf>, Vec<PathBuf>), AdapterError> {
    if yopts.frontend != SvFrontend::Verilog {
        return Ok((None, None, sources));
    }
    let bin = locate_sv2v()?;
    let preprocessed = dir.join("preprocessed.sv");
    // Include-search path, in priority order: the staging tempdir itself (every
    // `additional_sources` entry is written there, so an `\`include "X.sv"` whose target is a
    // staged source resolves — sv2v does not search the including file's own directory), the
    // parent dir of the primary source on disk (header files beside the `.sv` the user invoked
    // us on), then the caller-supplied include dirs.
    let mut include_dirs: Vec<PathBuf> = vec![dir.to_path_buf()];
    include_dirs.extend(yopts.primary_source_path.as_ref().and_then(|p| {
        let abs = Path::new(p)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(p));
        abs.parent().map(Path::to_path_buf)
    }));
    include_dirs.extend(yopts.extra_include_dirs.iter().cloned());
    let plan = Sv2vPlan {
        bin,
        sources,
        include_dirs,
        out: preprocessed,
    };
    // yosys reads the single combined Verilog-2005 file by its bare name, from inside the
    // staging directory (mununu#806).
    Ok((
        Some(plan),
        Some(dir.to_path_buf()),
        vec![PathBuf::from("preprocessed.sv")],
    ))
}

/// The oracle's read commands: one `read_verilog -formal -sv` per source (`test_support`).
#[cfg(feature = "test_support")]
fn oracle_read_cmds(sources: &[PathBuf], extra_include_dirs: &[PathBuf]) -> Vec<String> {
    let extra_inc: String = extra_include_dirs
        .iter()
        .map(|d| format!(" -I{}", d.display()))
        .collect();
    sources
        .iter()
        .map(|p| format!("read_verilog -formal -sv{extra_inc} {}", p.display()))
        .collect()
}

fn run_sv_flatten_btor2_concrete(
    content: &str,
    yopts: &YosysOptions,
) -> Result<SvFlattenArtifacts, AdapterError> {
    let yosys = locate_yosys()?;
    let tmp = TempDir::new("mununu-yosys")?;
    let plan = plan_lift(content, yopts, tmp.path())?;
    for (path, text) in &plan.staged {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        write_file(path, text)?;
    }
    #[cfg(feature = "test_support")]
    if let Some(s) = &plan.sv2v {
        run_sv2v(&s.bin, &s.sources, &s.include_dirs, &s.out)?;
    }

    // `-q` keeps the console to warnings and errors — and silences sv-elab's diagnostics, which
    // go through yosys's `log()`, so a failed `read_slang` reached the report as nothing but
    // "Design elaboration failed; see full log for details" (2026-11-04: seven corpus designs,
    // no reason named). `-l` keeps the full log beside the BTOR2 and the failure path reads it.
    let log_path = plan.btor_path.with_file_name("yosys.log");
    let run_script = |script: &str| -> Result<std::process::Output, AdapterError> {
        let mut yosys_cmd = Command::new(&yosys);
        if std::env::var_os("MUNUNU_DEBUG_YOSYS_SCRIPT").is_some() {
            eprintln!(
                "MUNUNU_DEBUG yosys cwd={:?}\nMUNUNU_DEBUG script={script}",
                plan.yosys_cwd
            );
        }
        if let Some(cwd) = &plan.yosys_cwd {
            yosys_cmd.current_dir(cwd);
        }
        if let Some(plugin) = &plan.plugin {
            yosys_cmd.arg("-m").arg(plugin);
        }
        yosys_cmd
            .arg("-l")
            .arg(&log_path)
            .arg("-q")
            .arg("-p")
            .arg(script);
        // N0.2 — the lift cache's manifest renders this invocation as the script text, the
        // plugin identity and the cwd rule. An argument or environment variable added here
        // that the script text does not carry needs `lift_cache::LIFT_CACHE_FORMAT` bumped,
        // or entries stored before the change would be served after it (`-l` only adds a
        // log the cache never stores, and changes no output).
        // mununu#504 — bounded. This ran as a bare `.output()`, i.e. forever. The lift precedes
        // every property, so a hung yosys emits ZERO output and the whole run reads as absent —
        // one of the two ways a consumer loses a full gate run. `run_yosys` 700 lines below has
        // used this same helper for the per-module path all along.
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
        Ok(std::process::Output {
            status,
            stdout: stdout_s.into_bytes(),
            stderr: stderr_s.into_bytes(),
        })
    };
    let mut output = run_script(&plan.script)?;
    let mut backend_retry = None;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // The BTOR backend walks the netlist cell by cell and reports a "topological loop" where
        // `check` sees none: yosys-slang's `$buf` cells chain wide vectors bit-wise through
        // other cells (OpenTitan csrng, 2026-10-22: 4,035 `$buf` cells, the loop through the
        // command arbiter's grant vector). `opt_clean` folds the buffers into connections and
        // the backend's walk terminates; it removes nothing the design reads, so the model is
        // the same design. Retried once, and only on that error, so every lift that succeeds
        // today is byte-identical.
        if let Some(retry) = backend_loop_retry_script(&plan.script, &stderr) {
            let second = run_script(&retry)?;
            if second.status.success() {
                backend_retry = Some(format!(
                    "the BTOR backend reported a topological loop ({}); the lift succeeded with \
                     `opt_clean` before `write_btor`",
                    first_line(&stderr)
                ));
                output = second;
            }
        }
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let log_errors = yosys_log_errors(&log_path);
        let hint = read_slang_hint(&format!("{stderr}\n{log_errors}"));
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: yosys exited with status {}{hint} \nstdout:\n{stdout}\nstderr:\n{stderr}{log_errors}",
                output.status
            ),
            location: None,
        });
    }

    let btor2 = std::fs::read_to_string(&plan.btor_path).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: yosys ran but did not produce {} ({e})",
            plan.btor_path.display()
        ),
        location: None,
    })?;

    // The hierarchy snapshot (port directions + blackbox scan) and the flattened netlist (the
    // clock-domain scan) are written by every script `build_script` renders. One that is
    // missing after yosys succeeded is a lift that went wrong, not an empty design: read as
    // empty, it switched off the multi-clock refusal and the black-box scan silently, and the
    // lift cache stored that (mununu#957). A lift error instead — never cached.
    let hier_json = read_lift_snapshot(&plan.hier_json_path, "hierarchy")?;

    // The BTOR2 model has ONE implicit clock: every `next` fires on it. A design whose flops sit
    // in two domains was lifted as if they ticked together — silently, because the old refusal
    // matched clock-shaped input NAMES (`clk`, `clk_i`, …) and `clk_src`/`clk_dst` passed.
    let flat_netlist_json =
        read_lift_snapshot(&clock_json_path(&plan.btor_path), "flattened netlist")?;
    let clock_domains = clock_domains(&flat_netlist_json);
    refuse_merged_clock_domains(&clock_domains, yopts)?;

    Ok(SvFlattenArtifacts {
        btor2,
        hier_json,
        staged_primary: plan.primary.to_string_lossy().into_owned(),
        frontend_used: plan.frontend_used,
        backend_retry,
        clock_domains,
        flat_netlist_json,
        cache: LiftCacheOutcome::Off,
    })
}

/// A JSON snapshot the lift's script writes, read back; missing or empty is a lift error that
/// names the file (mununu#957).
fn read_lift_snapshot(path: &Path, what: &str) -> Result<String, AdapterError> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => Ok(text),
        Ok(_) => Err(lift_snapshot_error(path, what, "it is empty")),
        Err(e) => Err(lift_snapshot_error(path, what, &e.to_string())),
    }
}

fn lift_snapshot_error(path: &Path, what: &str, why: &str) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: yosys ran but its {what} snapshot {} cannot be used ({why}); without \
             it the multi-clock refusal and the black-box scan would be skipped",
            path.display()
        ),
        location: None,
    }
}

/// The multi-clock refusal: more than one domain is an error unless
/// [`YosysOptions::merge_clock_domains`] accepts the under-approximation. Applied to a fresh
/// lift and to a cache hit alike (N0.2), since the cache stores the netlist, not the decision.
fn refuse_merged_clock_domains(
    domains: &[ClockDomain],
    yopts: &YosysOptions,
) -> Result<(), AdapterError> {
    if domains.len() > 1 && !yopts.merge_clock_domains {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "adapter/yosys: the design has {} clock domains ({}); the model has one implicit \
                 clock, so lifting it would step every domain together and hide every crossing \
                 defect. Run `mununu sv cdc-lint` to classify the crossings, verify each domain \
                 on its own, or pass `--merge-clock-domains` to accept that under-approximation \
                 explicitly.",
                domains.len(),
                describe_clock_domains(domains)
            ),
            location: None,
        });
    }
    Ok(())
}

/// The yosys BTOR backend's loop diagnostic, as its first line reads (the "Active cells" list
/// that follows is yosys-internal cell names, useless to a reader).
fn first_line(stderr: &str) -> &str {
    stderr
        .lines()
        .find(|l| l.contains("topological loop"))
        .unwrap_or("")
        .trim()
}

/// The lift script with `opt_clean` spliced in before `write_btor`, when `stderr` is the BTOR
/// backend's `Found topological loop` error — `None` for any other failure (those are not
/// retried). `opt_clean` only folds `$buf` cells into connections and drops unused cells, so the
/// retried model is the same design; the splice is textual on the one `write_btor` command the
/// script ends with.
fn backend_loop_retry_script(script: &str, stderr: &str) -> Option<String> {
    if !stderr.contains("Found topological loop") {
        return None;
    }
    let idx = script.rfind("; write_btor ")?;
    Some(format!("{}; opt_clean{}", &script[..idx], &script[idx..]))
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

/// Drive the slang lift → Yosys-flatten → BTOR2 and return the design's named state
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

/// Run the flattened Yosys script and return *just*
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
    Ok(SvLiftResult {
        btor2: artifacts.btor2,
        blackboxed_modules: blackboxed_module_names(&artifacts.hier_json),
        frontend_used: artifacts.frontend_used,
        backend_retry: artifacts.backend_retry,
        clock_domains: artifacts.clock_domains,
        flat_netlist_json: artifacts.flat_netlist_json,
        lift_cache: artifacts.cache,
    })
}

/// The SV → BTOR2 lift result: the BTOR2 text, the modules the lift could not
/// model (blackboxed / undefined cells), and the front end that produced it.
pub struct SvLiftResult {
    pub btor2: String,
    pub blackboxed_modules: Vec<String>,
    /// The front end used: `Slang` on every product lift; `Verilog` only from the dev-only
    /// oracle, whose sv2v pass rewrites the source before yosys reads it (the netlist's `src`
    /// lines are then sv2v's output's, not the RTL's — the source map is declared unavailable).
    pub frontend_used: SvFrontend,
    /// `Some(reason)` when the BTOR backend's topological-loop error made the lift rerun its
    /// script with `opt_clean` before `write_btor` (the model is the same design).
    pub backend_retry: Option<String>,
    /// The flattened design's clock domains; more than one only under
    /// [`YosysOptions::merge_clock_domains`].
    pub clock_domains: Vec<ClockDomain>,
    /// The flattened netlist (yosys `write_json`, after `flatten` + `opt_clean`, before
    /// `async2sync`): every storage cell still carries its own clock, which `sv cdc-lint` reads.
    pub flat_netlist_json: String,
    /// What the lift cache (`MUNUNU_LIFT_CACHE_DIR`, N0.2) did for this lift: `Off` when unset,
    /// `Hit` (no tool ran), `Miss` (the tools ran, the result is stored) or `Bypassed(why)`.
    pub lift_cache: LiftCacheOutcome,
}

/// One clock domain of a flattened design: a clock net and the edge its storage cells fire on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockDomain {
    /// The net's name (a public name when the net has one).
    pub net: String,
    pub posedge: bool,
    /// How many storage cells (flops and memory ports) fire on it.
    pub cells: usize,
}

/// `clk_a (posedge, 3 cells), clk_b (posedge, 1 cell)`.
pub fn describe_clock_domains(domains: &[ClockDomain]) -> String {
    domains
        .iter()
        .map(|d| {
            format!(
                "{} ({}, {} cell{})",
                d.net,
                if d.posedge { "posedge" } else { "negedge" },
                d.cells,
                if d.cells == 1 { "" } else { "s" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The post-flatten netlist snapshot the clock-domain scan reads, beside the BTOR2 output.
fn clock_json_path(btor_out: &Path) -> PathBuf {
    btor_out.with_file_name("clocks.json")
}

/// Bit id → a net name for one module of a yosys `write_json` netlist, public names preferred
/// over `hide_name` ones; a bit of a multi-bit net is `name[i]`.
pub(crate) fn net_names(body: &serde_json::Value) -> HashMap<i64, String> {
    let mut names: HashMap<i64, (bool, String)> = HashMap::new();
    if let Some(nets) = body.get("netnames").and_then(|n| n.as_object()) {
        for (name, net) in nets {
            let hidden = net.get("hide_name").and_then(|h| h.as_i64()) == Some(1);
            let Some(bits) = net.get("bits").and_then(|b| b.as_array()) else {
                continue;
            };
            for (i, bit) in bits.iter().enumerate() {
                let Some(id) = bit.as_i64() else { continue };
                let label = if bits.len() == 1 {
                    name.clone()
                } else {
                    format!("{name}[{i}]")
                };
                let better = match names.get(&id) {
                    None => true,
                    Some((was_hidden, _)) => *was_hidden && !hidden,
                };
                if better {
                    names.insert(id, (hidden, label));
                }
            }
        }
    }
    names.into_iter().map(|(id, (_, n))| (id, n)).collect()
}

/// The top-level alias of every hierarchical net in the flattened netlist (`flat_netlist_json`)
/// that carries the same bits and the same leaf name as a top-level net: a checker bound with
/// `bind m m_sva u_sva (.*)` gives the design's `state_q` a second name, `u_sva.state_q`, and the
/// BTOR2 state can come out under either. Only names change; the bits are identical by
/// construction. Display only ([`crate::trace::TraceNames`]): the lifted model keeps its names.
pub fn trace_aliases(flat_netlist_json: &str) -> std::collections::BTreeMap<String, String> {
    let mut aliases = std::collections::BTreeMap::new();
    let Ok(json) = serde_json::from_str::<serde_json::Value>(flat_netlist_json) else {
        return aliases;
    };
    let Some(modules) = json.get("modules").and_then(|m| m.as_object()) else {
        return aliases;
    };
    let top = modules
        .values()
        .find(|m| m.get("attributes").and_then(|a| a.get("top")).is_some());
    let Some(nets) = top
        .or_else(|| {
            (modules.len() == 1)
                .then(|| modules.values().next())
                .flatten()
        })
        .and_then(|m| m.get("netnames"))
        .and_then(|n| n.as_object())
    else {
        return aliases;
    };
    let public = |net: &serde_json::Value| net.get("hide_name").and_then(|h| h.as_i64()) != Some(1);
    let top_level: HashMap<(&str, String), &str> = nets
        .iter()
        .filter(|(name, net)| !name.contains('.') && public(net))
        .map(|(name, net)| ((name.as_str(), net["bits"].to_string()), name.as_str()))
        .collect();
    for (name, net) in nets {
        let Some((_, leaf)) = name.rsplit_once('.') else {
            continue;
        };
        if let Some(alias) = top_level.get(&(leaf, net["bits"].to_string())) {
            aliases.insert(name.clone(), (*alias).to_string());
        }
    }
    aliases
}

/// Where each design signal lives in the RTL (2026-10-29), from yosys's `src` attributes in the
/// flattened netlist (`flat_netlist_json`): a net's declaration and, for a register, the `always`
/// block(s) of the flop(s) that drive it (the cell's `src`, joined through the `Q` bits). Ports are
/// `input` / `output`, a net a flop drives is a `register`; other nets are left out, as are
/// yosys-internal (`$…`, hidden) names. Only `--frontend slang` stamps the original file and line
/// (sv2v rewrites the source first); the caller declares the map unavailable on that path.
pub fn cell_sources(flat_netlist_json: &str) -> Vec<crate::trace::CellSource> {
    use crate::trace::{CellSource, SourceLoc};
    let Ok(json) = serde_json::from_str::<serde_json::Value>(flat_netlist_json) else {
        return Vec::new();
    };
    let Some(modules) = json.get("modules").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let top = modules
        .values()
        .find(|m| m.get("attributes").and_then(|a| a.get("top")).is_some())
        .or_else(|| {
            (modules.len() == 1)
                .then(|| modules.values().next())
                .flatten()
        });
    let Some(top) = top else {
        return Vec::new();
    };
    let Some(nets) = top.get("netnames").and_then(|n| n.as_object()) else {
        return Vec::new();
    };
    let public = |name: &str, net: &serde_json::Value| {
        !name.starts_with('$') && net.get("hide_name").and_then(|h| h.as_i64()) != Some(1)
    };
    let src_of = |v: &serde_json::Value| {
        v.get("attributes")
            .and_then(|a| a.get("src"))
            .and_then(|s| s.as_str())
            .and_then(SourceLoc::parse_yosys_src)
    };
    // Every public net carrying a bit.
    let mut by_bit: HashMap<i64, Vec<&str>> = HashMap::new();
    for (name, net) in nets {
        if !public(name, net) {
            continue;
        }
        for b in net
            .get("bits")
            .and_then(|b| b.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(b) = b.as_i64() {
                by_bit.entry(b).or_default().push(name.as_str());
            }
        }
    }
    // The registers: the `Q` bits of every storage cell, with the cell's `src`.
    const FLOPS: [&str; 14] = [
        "$dff",
        "$dffe",
        "$adff",
        "$adffe",
        "$sdff",
        "$sdffe",
        "$sdffce",
        "$aldff",
        "$aldffe",
        "$dffsr",
        "$dffsre",
        "$dlatch",
        "$adlatch",
        "$dlatchsr",
    ];
    let mut assigned: std::collections::BTreeMap<&str, Vec<SourceLoc>> =
        std::collections::BTreeMap::new();
    for cell in top
        .get("cells")
        .and_then(|c| c.as_object())
        .into_iter()
        .flat_map(|c| c.values())
    {
        let ty = cell.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !FLOPS.contains(&ty) {
            continue;
        }
        let loc = src_of(cell);
        for b in cell
            .get("connections")
            .and_then(|c| c.get("Q"))
            .and_then(|q| q.as_array())
            .into_iter()
            .flatten()
        {
            let Some(b) = b.as_i64() else {
                continue;
            };
            for n in by_bit.get(&b).into_iter().flatten() {
                let e = assigned.entry(n).or_default();
                if let Some(l) = &loc
                    && !e.contains(l)
                {
                    e.push(l.clone());
                }
            }
        }
    }
    let ports = top.get("ports").and_then(|p| p.as_object());
    let direction = |name: &str| {
        ports
            .and_then(|p| p.get(name))
            .and_then(|p| p.get("direction"))
            .and_then(|d| d.as_str())
    };
    let mut out = Vec::new();
    for (name, net) in nets {
        if !public(name, net) {
            continue;
        }
        let kind = match (direction(name), assigned.contains_key(name.as_str())) {
            (Some("input"), _) => "input",
            (_, true) => "register",
            (Some("output"), false) => "output",
            _ => continue,
        };
        out.push(CellSource {
            cell: name.clone(),
            kind: kind.to_string(),
            declared: src_of(net),
            assigned: assigned.remove(name.as_str()).unwrap_or_default(),
        });
    }
    out
}

/// The lift stages the primary source as `work.sv` in its temp directory, so yosys's `src`
/// attributes name that file; the report names the file the user invoked mununu on. `primary`
/// is the primary source's name as the caller gave it (a path on the CLI, `source_name` or
/// `<top>.sv` over HTTP); its basename replaces `work.sv` in every location.
pub fn name_staged_primary(sources: &mut [crate::trace::CellSource], primary: &str) {
    let file = std::path::Path::new(primary)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(primary)
        .to_string();
    for s in sources {
        for loc in s.declared.iter_mut().chain(s.assigned.iter_mut()) {
            if loc.file == "work.sv" {
                loc.file = file.clone();
            }
        }
    }
}

/// The clock domains of a flattened design, from a `write_json` snapshot taken after `flatten`
/// and before `async2sync`, on a copy cleaned by `opt_clean` (`design -save` / `-load` around it,
/// so the lifted model is untouched). The cleaning matters: right after `flatten` a sub-instance's
/// clock port is still its own wire connected to the parent's (`u_dec.clk_i` beside `clk_i`), and
/// yosys-slang routes nets through `$buf` cells; uncleaned, one clock reads as two domains
/// (OpenTitan `prim_esc_sender`, measured 2026-10-23). A domain is a distinct (clock net, edge)
/// a storage cell fires on — the `CLK` pin of a flop (`$dff`, `$adff`, `$dffe`, `$sdff`, `$aldff`, `$dffsr`, … with `CLK_POLARITY`)
/// and every clocked port of a memory (`$mem_v2` `RD_CLK` / `WR_CLK` where the port's
/// `*_CLK_ENABLE` bit is set). A constant clock bit is not a domain. Sorted by net, then edge.
pub fn clock_domains(flat_json: &str) -> Vec<ClockDomain> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(flat_json) else {
        return Vec::new();
    };
    let Some(modules) = root.get("modules").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    // A yosys parameter is a binary string, most significant bit first.
    let param_bit = |cell: &serde_json::Value, name: &str, i: usize| -> Option<bool> {
        let bits = cell.get("parameters")?.get(name)?.as_str()?;
        let b = bits.as_bytes();
        (i < b.len()).then(|| b[b.len() - 1 - i] == b'1')
    };
    let mut count: std::collections::BTreeMap<(String, bool), usize> = Default::default();
    for body in modules.values() {
        let names = net_names(body);
        let net_of = |bit: &serde_json::Value| -> Option<String> {
            let id = bit.as_i64()?; // "0" / "1" / "x" are constants, not nets
            Some(
                names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("net#{id}")),
            )
        };
        let Some(cells) = body.get("cells").and_then(|c| c.as_object()) else {
            continue;
        };
        for cell in cells.values() {
            let ty = cell.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let conns = cell.get("connections");
            if ty.starts_with("$mem") {
                for (clk, en, pol) in [
                    ("RD_CLK", "RD_CLK_ENABLE", "RD_CLK_POLARITY"),
                    ("WR_CLK", "WR_CLK_ENABLE", "WR_CLK_POLARITY"),
                ] {
                    let Some(bits) = conns.and_then(|c| c.get(clk)).and_then(|b| b.as_array())
                    else {
                        continue;
                    };
                    for (i, bit) in bits.iter().enumerate() {
                        if param_bit(cell, en, i) != Some(true) {
                            continue;
                        }
                        if let Some(net) = net_of(bit) {
                            let edge = param_bit(cell, pol, i).unwrap_or(true);
                            *count.entry((net, edge)).or_default() += 1;
                        }
                    }
                }
                continue;
            }
            let Some(bit) = conns
                .and_then(|c| c.get("CLK"))
                .and_then(|b| b.as_array())
                .and_then(|b| b.first())
            else {
                continue;
            };
            if let Some(net) = net_of(bit) {
                let edge = param_bit(cell, "CLK_POLARITY", 0).unwrap_or(true);
                *count.entry((net, edge)).or_default() += 1;
            }
        }
    }
    count
        .into_iter()
        .map(|((net, posedge), cells)| ClockDomain {
            net,
            posedge,
            cells,
        })
        .collect()
}

/// Scan the pre-flatten hierarchy JSON for cells whose `type` references a
/// module with no definition in the design (instantiated, no body) and is not a
/// Yosys built-in (`$...`) cell. These dangling cell references are NOT declared
/// blackbox *modules*, so [`parse_blackbox_modules`] misses them, yet they are
/// the dominant real-world "cut FSM" cause (OpenTitan's `prim_sparse_fsm_flop`).
/// Returned sorted + deduped.
fn parse_undefined_module_cells(hier_json: &str) -> Vec<String> {
    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(modules) = root.get("modules").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let defined: std::collections::HashSet<&str> = modules.keys().map(String::as_str).collect();
    instantiated_cell_types(hier_json)
        .iter()
        .filter(|ty| !defined.contains(ty.as_str()))
        .cloned()
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Every non-built-in cell TYPE instantiated anywhere in the elaborated design (`modules[*].cells
/// [*].type`, `$…` Yosys primitives excluded).
fn instantiated_cell_types(hier_json: &str) -> std::collections::BTreeSet<String> {
    let mut types = std::collections::BTreeSet::new();
    let root: serde_json::Value = match serde_json::from_str(hier_json) {
        Ok(v) => v,
        Err(_) => return types,
    };
    let Some(modules) = root.get("modules").and_then(|m| m.as_object()) else {
        return types;
    };
    for body in modules.values() {
        let Some(cells) = body.get("cells").and_then(|c| c.as_object()) else {
            continue;
        };
        for cell in cells.values() {
            if let Some(ty) = cell.get("type").and_then(|t| t.as_str())
                && !ty.starts_with('$')
            {
                types.insert(ty.to_string());
            }
        }
    }
    types
}

/// The modules the lift could not model, for `verify_auto`'s `blackboxed_modules` diagnostic:
/// the `(* blackbox *)`-marked modules that are actually INSTANTIATED under the top (mununu#724
/// — yosys marks any bodiless module blackbox, including an empty `module` that only carries
/// `@mununu_guarantee` comments and nothing instantiates; reporting it was a false soundness
/// caveat), plus the instantiated cell types with no definition at all. Sorted, deduped.
pub(crate) fn blackboxed_module_names(hier_json: &str) -> Vec<String> {
    let instantiated = instantiated_cell_types(hier_json);
    let mut names: std::collections::BTreeSet<String> = parse_blackbox_modules(hier_json)
        .into_iter()
        .map(|b| b.name)
        .filter(|name| instantiated.contains(name))
        .collect();
    names.extend(parse_undefined_module_cells(hier_json));
    names.into_iter().collect()
}

/// The module attribute that marks a stub standing in for a module the source set lacks.
/// sv-elab refuses an instantiation with no definition (2026-11-04); `read_verilog` lifted it
/// with the cell undefined, and yosys's BTOR backend made the cell's outputs anonymous inputs —
/// free, non-deliberate leaves that `violation-over-free-leaves` withholds a VIOLATED over. A
/// stub carrying this attribute (empty, with the module's ports) gets exactly that: the read keeps
/// it as a black box (`--empty-blackboxes`), the hierarchy snapshot names it in
/// `blackboxed_modules`, and the script deletes the module before the BTOR2 is written, so the
/// parent's cell is undefined again. Measured 2026-11-04: two anonymous inputs, no next-less
/// state — the old reader's shape to the byte; without the attribute the same stub is a
/// deliberate `(* blackbox *)` cut (`cutpoint -blackbox`, next-less states, verdict kept).
pub const MISSING_MODULE_ATTR: &str = "mununu_missing_module";

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
    let plugin = slang_plugin_arg();
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

    // Pass 1 — discovery. Use `hierarchy -auto-top` when the caller did
    // not provide one (matches the legacy build_script).
    let hier_json_path = tmp.path().join("hier.json");
    let discovery_script = build_discovery_script(
        &sources,
        yopts.top.as_deref(),
        &hier_json_path,
        &yopts.params,
    );
    run_yosys(
        &yosys,
        &discovery_script,
        PER_MODULE_YOSYS_TIMEOUT,
        plugin.as_deref(),
        &hier_json_path.with_file_name("yosys-discovery.log"),
    )?;

    let hier_body = std::fs::read_to_string(&hier_json_path).map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!(
            "adapter/yosys: per-module discovery pass succeeded but hierarchy snapshot {} is unreadable ({e})",
            hier_json_path.display()
        ),
        location: None,
    })?;
    let hier_body = collapse_slang_instance_modules(&hier_body);
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
        run_yosys(
            &yosys,
            &script,
            PER_MODULE_YOSYS_TIMEOUT,
            plugin.as_deref(),
            &btor_path.with_file_name(format!("{module_name}.yosys.log")),
        )?;

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
/// passing `--top` makes the target explicit. The dev-only oracle's mechanism (`test_support`).
#[cfg(feature = "test_support")]
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
    // mununu#981 — `read_slang` flattens the elaborated design into the top by default, so the
    // snapshot had ONE module and the per-module / multi-module lift found no submodule.
    // `--keep-hierarchy` keeps one module per instance, named `<type>$<instance path>`;
    // [`collapse_slang_instance_modules`] maps those back to the module type the per-module pass
    // re-elaborates.
    let read = format!(
        "{} --keep-hierarchy",
        read_slang_cmd(sources, top, params, &[])
    );
    let hier = match top {
        Some(t) => format!("hierarchy -check -top {t}"),
        None => "hierarchy -check -auto-top".to_string(),
    };
    format!(
        "{read}; {hier}; proc; write_json {}",
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
    // Parameter overrides apply to this submodule's own top (`module`) at read time (`-G`).
    let read = read_slang_cmd(sources, Some(module), params, &[]);
    let setundef_pass = select_setundef_pass(setundef_anyseq, setundef_anyconst);
    let per_signal = emit_init_policy_setattrs(init_policy_overrides);
    format!(
        // `memory_collect` normalizes memory read/write accesses into `$mem` cells
        // (fixing an internal yosys assert on some fifo styles when a raw `$mem`
        // reaches `write_btor`) while KEEPING the memory as a BTOR2 array — the
        // bit-blaster resolves array sorts, so this stays scalable (no map-to-flops
        // explosion). Unlike `memory`/`memory -nomap`, it runs NO internal `opt_clean`,
        // so it does not drop dead registers (which would change memory-free lifts).
        // No-op when the design has no memories.
        "{read}; hierarchy -check -top {module}; {per_signal}proc; memory_collect; flatten; async2sync; chformal -lower; dffunmap; {setundef_pass}; write_btor {}",
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

/// Spawn Yosys with the given script. Captures stdout/stderr — and the `error` lines of the
/// full log kept at `log` (`-l`; `-q` keeps sv-elab's diagnostics off the console) — into the
/// `AdapterError` message on failure.
fn run_yosys(
    yosys: &Path,
    script: &str,
    timeout: Duration,
    plugin: Option<&str>,
    log: &Path,
) -> Result<(), AdapterError> {
    let mut command = Command::new(yosys);
    if let Some(p) = plugin {
        command.arg("-m").arg(p);
    }
    command.arg("-l").arg(log).arg("-q").arg("-p").arg(script);
    let outcome = crate::adapter::run_with_timeout(&mut command, None, timeout).map_err(|e| {
        AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/yosys: failed to spawn yosys: {e}"),
            location: None,
        }
    })?;
    yosys_run_outcome_to_result(outcome, script, timeout, &yosys_log_errors(log))
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
    log_errors: &str,
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
        let hint = read_slang_hint(&format!("{stderr}\n{log_errors}"));
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys: yosys exited with status {status}{hint} (script: {script})\nstdout:\n{stdout}\nstderr:\n{stderr}{log_errors}"
            ),
            location: None,
        });
    }
    Ok(())
}

/// The module type of a `read_slang --keep-hierarchy` module name: yosys-slang names each
/// instance's module `<type>$<instance path>` (`producer$producer_consumer_top.u_producer`).
/// A name with no `$`, or one yosys itself mangled (`$paramod…`, leading `$`), is returned as is.
fn slang_module_type(name: &str) -> &str {
    if name.starts_with('$') {
        return name;
    }
    name.split_once('$').map_or(name, |(ty, _)| ty)
}

/// mununu#981 — rewrite a `read_slang --keep-hierarchy` snapshot so every per-instance module
/// `<type>$<path>` becomes its type: each cell's `type` is mapped, and the `modules` map keeps one
/// body per type (the first by name — the map is sorted; the other instances' bodies are dropped,
/// their cells still point at the type). The per-module pass then re-elaborates each TYPE with
/// `read_slang --top <type>`, the shape the `read_verilog` discovery produced before the
/// slang-only lift (N1). A snapshot that fails to parse is returned unchanged (the readers are
/// best-effort on it already).
fn collapse_slang_instance_modules(hier_json: &str) -> String {
    let Ok(mut root) = serde_json::from_str::<serde_json::Value>(hier_json) else {
        return hier_json.to_string();
    };
    let Some(modules) = root.get_mut("modules").and_then(|m| m.as_object_mut()) else {
        return hier_json.to_string();
    };
    if !modules.keys().any(|k| slang_module_type(k) != k) {
        return hier_json.to_string();
    }
    let mut collapsed = serde_json::Map::new();
    for (name, mut body) in std::mem::take(modules) {
        if let Some(cells) = body.get_mut("cells").and_then(|c| c.as_object_mut()) {
            for cell in cells.values_mut() {
                if let Some(ty) = cell.get_mut("type")
                    && let Some(t) = ty.as_str()
                {
                    let mapped = slang_module_type(t).to_string();
                    *ty = serde_json::Value::String(mapped);
                }
            }
        }
        collapsed
            .entry(slang_module_type(&name).to_string())
            .or_insert(body);
    }
    *modules = collapsed;
    root.to_string()
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
    if let Some(path) = crate::adapter::env_var::read("MUNUNU_YOSYS_PATH") {
        return Ok(PathBuf::from(path));
    }
    let candidates = ["yosys"];
    for c in candidates {
        if let Ok((status, _, _)) = crate::adapter::probe_tool(Path::new(c), &["-V"])
            && status.success()
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

/// Find a usable sv2v binary for the dev-only oracle (`test_support`): `MUNUNU_SV2V_PATH`
/// first, then the bare `sv2v` on `$PATH`. zachjs/sv2v's `--version` flag is a stable smoke test.
#[cfg(feature = "test_support")]
fn locate_sv2v() -> Result<PathBuf, AdapterError> {
    if let Some(path) = crate::adapter::env_var::read("MUNUNU_SV2V_PATH") {
        return Ok(PathBuf::from(path));
    }
    if let Ok((status, _, _)) = crate::adapter::probe_tool(Path::new("sv2v"), &["--version"])
        && status.success()
    {
        return Ok(PathBuf::from("sv2v"));
    }
    Err(AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: "adapter/yosys: sv2v binary not found in $PATH (set MUNUNU_SV2V_PATH or install zachjs/sv2v ≥ 0.0.10) — the dev-only read_verilog oracle needs it"
            .into(),
        location: None,
    })
}

/// The yosys `-m` argument loading a yosys-slang plugin, when `MUNUNU_YOSYS_SLANG_PLUGIN`
/// names one (a path to `slang.so`, or a bare name yosys resolves from its plugin dir); `None`
/// otherwise — `read_slang` is built into yosys ≥ 0.67 and the pinned `mununu-sva` image, and
/// loading the plugin beside the built-in reader would register the command twice.
///
/// `read_slang` is a full SystemVerilog front end. mununu uses it for the **RTL lift only**
/// (`--ignore-assertions` — SVA is extracted separately by the slang `extract_sva` path, so no
/// `bad` nodes are needed here). Named registers and ports get `instance.signal` BTOR2 names,
/// so cutpoints / `--pin` over named signals resolve by name.
fn slang_plugin_arg() -> Option<String> {
    crate::adapter::env_var::read("MUNUNU_YOSYS_SLANG_PLUGIN").filter(|p| !p.trim().is_empty())
}

/// The actionable tail of a yosys failure, read off its stderr and log: `read_slang` is not a
/// command (the binary predates the built-in reader and no plugin was named), or sv-elab refused
/// an instantiated module it has no definition for — `read_verilog` black-boxed a missing module
/// silently (its outputs free, `diagnostics.blackboxed_modules` naming it); sv-elab retired
/// `--ignore-unknown-modules` and wants a definition, which a port-only `(* blackbox *)` stub
/// supplies with the same semantics (2026-11-04: seven corpus designs lifted only by the old
/// reader failed this way, the reason hidden by `-q`).
fn read_slang_hint(text: &str) -> String {
    let mut hint = String::new();
    if text.contains("No such command: read_slang") {
        hint.push_str(
            " — this yosys has no `read_slang`: use yosys ≥ 0.67 (the pinned oss-cad-suite of \
             the mununu-sva image has it built in) or set MUNUNU_YOSYS_SLANG_PLUGIN to a \
             yosys-slang plugin",
        );
    }
    if text.contains("unknown module '") {
        hint.push_str(&format!(
            " — the design instantiates a module with no definition: sv-elab needs one for every \
             instantiated module (`read_verilog` lifted a missing one silently; a known flop \
             primitive such as prim_flop / prim_sparse_fsm_flop is still stubbed automatically \
             unless --no-auto-stub-flops). Add the module's source, or an empty stub module with \
             its ports: marked `(* {MISSING_MODULE_ATTR} *)` it stands in for the missing module \
             exactly as the old reader did — outputs free, a VIOLATED over them withheld — and \
             unmarked (or `(* blackbox *)`) it is a deliberate cut that keeps its verdict under \
             the `blackbox-cut` caveat; either way `diagnostics.blackboxed_modules` names it"
        ));
    }
    hint
}

/// The module names a failed lift's message says have no definition — every
/// `unknown module 'NAME'` sv-elab wrote into the log ([`yosys_log_errors`] carries them into the
/// message), sorted and deduplicated. `verify_auto`'s H.C auto-stub reads them to stage the
/// behavioural stubs of the flop primitives it knows; under `read_verilog` the same names came
/// from the hierarchy snapshot of a lift that succeeded with the modules cut.
pub(crate) fn unknown_modules_in_error(message: &str) -> Vec<String> {
    const MARK: &str = "unknown module '";
    let mut names = std::collections::BTreeSet::new();
    let mut rest = message;
    while let Some(i) = rest.find(MARK) {
        rest = &rest[i + MARK.len()..];
        let Some(end) = rest.find('\'') else {
            break;
        };
        let name = &rest[..end];
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            names.insert(name.to_string());
        }
        rest = &rest[end..];
    }
    names.into_iter().collect()
}

/// The `error` lines of a yosys log kept with `-l`: sv-elab's `file:line:col: error: …`
/// diagnostics and yosys's own `ERROR: …`, in order, deduplicated, at most 24 — the reason a lift
/// failed, which `-q` keeps off the console. Empty when the log is missing or carries none.
fn yosys_log_errors(log: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(log) else {
        return String::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut lines: Vec<&str> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let is_error =
            line.contains(": error: ") || line.starts_with("ERROR:") || line.starts_with("error:");
        if is_error && seen.insert(line) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let total = lines.len();
    lines.truncate(24);
    let more = if total > lines.len() {
        format!("\n… {} more", total - lines.len())
    } else {
        String::new()
    };
    format!("\nyosys log (errors):\n{}{more}\n", lines.join("\n"))
}

/// Test-only probe shared by every lib test that lifts SystemVerilog: a `yosys` on the PATH
/// **with** `read_slang` (built in from 0.67, or loaded from `MUNUNU_YOSYS_SLANG_PLUGIN`). A
/// yosys without it (a brew 0.59) skips those tests, as a host without yosys always did; the
/// `mununu-sva` image runs them. `help <unknown>` exits 0 and prints "No such command or cell
/// type" — and `-q` silences that line — so the output is read, not the status alone.
#[cfg(test)]
pub(crate) fn yosys_can_read_slang() -> bool {
    let mut cmd = Command::new("yosys");
    if let Some(p) = slang_plugin_arg() {
        cmd.arg("-m").arg(p);
    }
    cmd.args(["-p", "help read_slang"])
        .output()
        .map(|o| {
            o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("No such command")
        })
        .unwrap_or(false)
}

/// sv2v's argument vector: `-I<dir>` per include dir, then the sources. One function for the
/// run ([`run_sv2v`]) and the lift cache's manifest (N0.2), so the key is what runs.
#[cfg(feature = "test_support")]
fn sv2v_args(sources: &[PathBuf], include_dirs: &[PathBuf]) -> Vec<String> {
    include_dirs
        .iter()
        .map(|dir| format!("-I{}", dir.display()))
        .chain(sources.iter().map(|s| s.display().to_string()))
        .collect()
}

/// Run sv2v over `sources`, capture stdout into `out`. sv2v's documented
/// multi-file mode resolves cross-file packages/interfaces in one pass
/// and emits a single combined Verilog-2005 stream on stdout.
///
/// `include_dirs` forwards `-I` flags so `\`include` directives in the
/// `.sv` sources (e.g. `\`include "caliptra_sva.svh"`) resolve. Without
/// these, sv2v errors on missing headers because mununu stages a per-call
/// tempdir away from the original source directory.
#[cfg(feature = "test_support")]
fn run_sv2v(
    sv2v: &Path,
    sources: &[PathBuf],
    include_dirs: &[PathBuf],
    out: &Path,
) -> Result<(), AdapterError> {
    let mut cmd = Command::new(sv2v);
    cmd.args(sv2v_args(sources, include_dirs));
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

/// `yosys -V` (stdout + stderr) for `yosys`, read once per process: the lift cache keys on it
/// (N0.2).
fn yosys_banner(yosys: &Path) -> Result<String, AdapterError> {
    tool_version(yosys, "-V", "yosys")
}

/// `sv2v --version` for `sv2v`, read once per process (the lift cache keys on it, N0.2).
#[cfg(feature = "test_support")]
fn sv2v_version(sv2v: &Path) -> Result<String, AdapterError> {
    tool_version(sv2v, "--version", "sv2v")
}

/// A tool's version banner, memoised per (binary, flag, size + mtime) for the life of the
/// process — a lift that asks for the same banner twice would otherwise spawn the tool twice.
/// The binary's stamp is in the memo key so a tool replaced while the process runs (a long-lived
/// server, an image rebuilt under it) is asked again (mununu#956).
fn tool_version(bin: &Path, flag: &str, label: &str) -> Result<String, AdapterError> {
    #[allow(clippy::type_complexity)]
    static BANNERS: OnceLock<Mutex<HashMap<(PathBuf, String, String), String>>> = OnceLock::new();
    let stamp = resolve_binary(bin)
        .and_then(|p| file_stamp(&p).ok())
        .unwrap_or_default();
    let key = (bin.to_path_buf(), flag.to_string(), stamp);
    let cache = BANNERS.get_or_init(Default::default);
    if let Some(banner) = cache.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return Ok(banner);
    }
    let (_, stdout, stderr) =
        crate::adapter::probe_tool(bin, &[flag]).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/yosys: failed to read {label} version: {e}"),
            location: None,
        })?;
    let banner = format!("{stdout}{stderr}");
    if let Ok(mut m) = cache.lock() {
        m.insert(key, banner.clone());
    }
    Ok(banner)
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

/// The `read_slang` command of a lift: every source in ONE read (slang elaborates the whole
/// design at read time), `--ignore-assertions` (embedded SVA is extracted separately by the
/// slang `extract_sva` path; the flag also drops *immediate* `assert` statements, so they
/// never become `bad` nodes — measured 2026-11-04 on yosys 0.68+120: without the flag
/// yosys-slang lowers immediate and boolean `assert property` to `$check` cells but refuses
/// any temporal property (`|=>`, `##1`, `$rose`, `s_eventually`) as "unsupported SVA feature",
/// a hard elaboration error, and offers no finer switch; sv2v dropped immediate assertions
/// too, so no product path ever lifted them), `--ignore-timing` (slang is stricter than `read_verilog` about
/// `#`-delay timing controls, which are not part of the design's cycle behaviour — SOUNDNESS:
/// read_verilog-equivalent, not an extra abstraction; without it ~18 corpus designs failed to
/// lift, 2026-07-25), `--empty-blackboxes` (an empty module is a black box — what yosys made of a
/// bodiless module under `read_verilog`, mununu#724 — rather than a module whose outputs read
/// constant zero, which is what sv-elab makes of it otherwise; measured 2026-11-04. A stub marked
/// [`MISSING_MODULE_ATTR`] is deleted again after the hierarchy snapshot), `--top` when given, one `-G NAME=VALUE` per parameter override (top-scoped,
/// so `MODULE.NAME` applies by its bare name; an unknown name is validated upstream against the
/// slang AST), the staging directory as an include dir (a cross-file `\`include "peer.sv"`
/// between staged sources resolves) and the caller's include dirs.
fn read_slang_cmd(
    sources: &[PathBuf],
    top: Option<&str>,
    params: &[(String, String)],
    extra_include_dirs: &[PathBuf],
) -> String {
    let files = sources
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let top_arg = top.map(|t| format!(" --top {t}")).unwrap_or_default();
    let inc = sources
        .first()
        .and_then(|p| p.parent())
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| format!(" -I{}", d.display()))
        .unwrap_or_default();
    let extra_inc: String = extra_include_dirs
        .iter()
        .map(|d| format!(" -I{}", d.display()))
        .collect();
    let param_g: String = params
        .iter()
        .map(|(lhs, val)| {
            let name = lhs.rsplit('.').next().unwrap_or(lhs);
            format!(" -G {name}={val}")
        })
        .collect();
    format!(
        "read_slang --ignore-assertions --ignore-timing --empty-blackboxes{top_arg}{param_g}{inc}{extra_inc} {files}"
    )
}

// The lift-script builder threads each independent Yosys-pass knob (top, output
// paths, the two setundef flags, per-signal init overrides, and now the cut points)
// as its own argument; bundling them into a struct would not improve clarity here.
#[allow(clippy::too_many_arguments)]
fn build_script(
    read_cmds: &[String],
    chparam: &str,
    top: Option<&str>,
    btor_out: &Path,
    hier_json_out: &Path,
    setundef_anyseq: bool,
    setundef_anyconst: bool,
    init_policy_overrides: &InitPolicyOverrides,
    cutpoint_signals: &[String],
) -> String {
    // The read command(s) come from the caller — [`read_slang_cmd`] for the product lift, the
    // oracle's `read_verilog` lines (with its `chparam` prefix) under `test_support` — and
    // everything after the read is identical, so the lift is front-end-agnostic downstream.
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
        // `delete =A:…` right after the snapshot: a stub marked [`MISSING_MODULE_ATTR`] has been
        // named in the hierarchy JSON and is now removed, so its cell is undefined when the BTOR2 is
        // written — the old reader's missing-module shape (anonymous inputs). The `=` includes
        // black-box modules in the selection; with no such module the pass is a no-op.
        "{}; {hier}; {per_signal}proc; write_json {}; delete =A:{MISSING_MODULE_ATTR}; {cutpoints}cutpoint -blackbox; memory_collect; flatten; design -save mununu_pre_clocks; opt_clean; write_json {}; design -load mununu_pre_clocks; async2sync; chformal -lower; dffunmap; {setundef_pass}; write_btor {}",
        read_cmds.join("; "),
        hier_json_out.display(),
        clock_json_path(btor_out).display(),
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
    /// Two flops on two clock nets are two domains; constant clock bits are none; a `$mem_v2`
    /// port counts only when its clock is enabled; a hidden name loses to a public one.
    #[test]
    fn clock_domains_counts_distinct_clock_nets_and_edges() {
        let json = r#"{"modules": {"top": {
            "netnames": {
                "clk_a": {"hide_name": 0, "bits": [2]},
                "$auto$x": {"hide_name": 1, "bits": [3]},
                "clk_b": {"hide_name": 0, "bits": [3]}
            },
            "cells": {
                "q1": {"type": "$dff", "parameters": {"CLK_POLARITY": "1"},
                       "connections": {"CLK": [2], "D": [5], "Q": [6]}},
                "q2": {"type": "$adff", "parameters": {"CLK_POLARITY": "00000001"},
                       "connections": {"CLK": [3], "D": [6], "Q": [7]}},
                "q3": {"type": "$dff", "parameters": {"CLK_POLARITY": "0"},
                       "connections": {"CLK": [2], "D": [7], "Q": [8]}},
                "k":  {"type": "$dff", "parameters": {"CLK_POLARITY": "1"},
                       "connections": {"CLK": ["0"], "D": [7], "Q": [9]}},
                "m":  {"type": "$mem_v2",
                       "parameters": {"RD_CLK_ENABLE": "0", "RD_CLK_POLARITY": "1",
                                      "WR_CLK_ENABLE": "1", "WR_CLK_POLARITY": "1"},
                       "connections": {"RD_CLK": [3], "WR_CLK": [2]}},
                "g":  {"type": "$and", "parameters": {}, "connections": {"A": [2], "B": [3], "Y": [10]}}
            }
        }}}"#;
        let d = clock_domains(json);
        assert_eq!(
            d,
            vec![
                ClockDomain {
                    net: "clk_a".into(),
                    posedge: false,
                    cells: 1
                },
                ClockDomain {
                    net: "clk_a".into(),
                    posedge: true,
                    cells: 2
                },
                ClockDomain {
                    net: "clk_b".into(),
                    posedge: true,
                    cells: 1
                },
            ]
        );
        assert_eq!(
            describe_clock_domains(&d[1..]),
            "clk_a (posedge, 2 cells), clk_b (posedge, 1 cell)"
        );
    }

    /// A bound checker's port name maps to the design's own net (same bits, same leaf), as in
    /// the flattened snapshot of `bind pump_seq pump_seq_sva u (.*)`. A hierarchical net on
    /// different bits, or with another leaf name, keeps its name, and garbage yields nothing.
    #[test]
    fn trace_aliases_map_a_bound_port_to_the_designs_net() {
        let json = r#"{"modules": {"pump_seq": {
            "attributes": {"top": "00000000000000000000000000000001"},
            "netnames": {
                "state_q": {"hide_name": 0, "bits": [11, 12, 13]},
                "u_pump_seq_sva.state_q": {"hide_name": 0, "bits": [11, 12, 13]},
                "u_pump_seq_sva.lof_q": {"hide_name": 0, "bits": [48, 49]},
                "lof_q": {"hide_name": 0, "bits": [22, 23]},
                "u_core.cnt": {"hide_name": 0, "bits": [11, 12, 13]}
            },
            "cells": {}
        }}}"#;
        let a = trace_aliases(json);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a["u_pump_seq_sva.state_q"], "state_q");
        assert!(trace_aliases("not json").is_empty());
    }

    /// One clock is one domain, and a snapshot that does not parse reports none.
    #[test]
    fn clock_domains_single_clock_and_garbage() {
        let json = r#"{"modules": {"top": {"netnames": {"clk": {"hide_name": 0, "bits": [2]}},
            "cells": {"q": {"type": "$sdff", "parameters": {"CLK_POLARITY": "1"},
                            "connections": {"CLK": [2]}}}}}}"#;
        assert_eq!(clock_domains(json).len(), 1);
        assert!(clock_domains("not json").is_empty());
    }

    use super::*;
    use crate::controllability::BoundaryDirection;

    /// The product lift's script for `sources` — `read_slang_cmd` + `build_script`, composed
    /// as `plan_lift` composes them; no staging, no tool.
    fn slang_script(
        sources: &[PathBuf],
        top: Option<&str>,
        cuts: &[String],
        inc: &[PathBuf],
        params: &[(String, String)],
    ) -> String {
        let overrides: InitPolicyOverrides = Vec::new();
        build_script(
            &[read_slang_cmd(sources, top, params, inc)],
            "",
            top,
            Path::new("/tmp/out.btor"),
            Path::new("/tmp/hier.json"),
            false,
            false,
            &overrides,
            cuts,
        )
    }

    /// The dev-only oracle's script (`test_support`): `read_verilog` lines + `chparam`.
    #[cfg(feature = "test_support")]
    fn oracle_script(
        sources: &[PathBuf],
        top: Option<&str>,
        inc: &[PathBuf],
        params: &[(String, String)],
    ) -> String {
        let overrides: InitPolicyOverrides = Vec::new();
        build_script(
            &oracle_read_cmds(sources, inc),
            &build_chparam_passes(params, top),
            top,
            Path::new("/tmp/out.btor"),
            Path::new("/tmp/hier.json"),
            false,
            false,
            &overrides,
            &[],
        )
    }

    #[test]
    /// mununu#504 — a lift-subprocess timeout must produce an ACTIONABLE error naming the cap,
    /// not a bare failure. Pure mapper, so this runs without spawning anything.
    fn backend_loop_error_retries_with_opt_clean_before_write_btor_only() {
        let script = "read_slang --top t a.sv; hierarchy -top t; proc; write_json h.json; \
                      cutpoint -blackbox; memory_collect; flatten; async2sync; chformal -lower; \
                      dffunmap; setundef -zero; write_btor d.btor";
        let loop_err =
            "ERROR: Found topological loop while processing cell $6638. Active cells:\n    $6602\n";
        let retried = backend_loop_retry_script(script, loop_err).expect("the loop error retries");
        assert!(
            retried.ends_with("; setundef -zero; opt_clean; write_btor d.btor"),
            "{retried}"
        );
        assert_eq!(retried.matches("opt_clean").count(), 1);
        assert_eq!(
            backend_loop_retry_script(script, "ERROR: syntax error, unexpected TOK_DEFAULT"),
            None,
            "any other yosys error is not retried"
        );
        assert_eq!(
            first_line(loop_err),
            "ERROR: Found topological loop while processing cell $6638. Active cells:"
        );
    }

    #[test]
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

    /// A design whose flops sit on two clock nets is refused at lift time; it used to lift as if
    /// both domains ticked together, and only clock-shaped input NAMES were refused, so
    /// `clk_src` / `clk_dst` passed silently. `merge_clock_domains` lifts it and reports the
    /// domains; a one-clock design is untouched. Needs yosys ⇒ mununu-sva image.
    #[test]
    #[ignore = "requires yosys (mununu-sva image)"]
    fn e2e_two_clock_design_is_refused_at_lift() {
        const TWO_CLOCKS: &str = "\
module cdc (input logic clk_src, input logic clk_dst, input logic d, output logic q);
  logic a, b;
  always_ff @(posedge clk_src) a <= d;
  always_ff @(posedge clk_dst) b <= a;
  always_ff @(posedge clk_dst) q <= b;
endmodule
";
        let Err(err) = sv_to_btor2_with_blackboxes(TWO_CLOCKS, &YosysOptions::default()) else {
            panic!("a two-clock design must not lift as one clock");
        };
        assert!(
            err.message.contains("2 clock domains")
                && err.message.contains("clk_src (posedge, 1 cell)")
                && err.message.contains("clk_dst (posedge, 2 cells)"),
            "{}",
            err.message
        );
        let merged = sv_to_btor2_with_blackboxes(
            TWO_CLOCKS,
            &YosysOptions {
                merge_clock_domains: true,
                ..Default::default()
            },
        )
        .expect("the explicit opt-in lifts it");
        assert_eq!(merged.clock_domains.len(), 2);
        let one = sv_to_btor2_with_blackboxes(
            &TWO_CLOCKS.replace("clk_dst)", "clk_src)"),
            &YosysOptions::default(),
        )
        .expect("one clock lifts");
        assert_eq!(one.clock_domains.len(), 1);
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
        let err = yosys_run_outcome_to_result(None, "write_btor …", Duration::from_secs(60), "")
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

    /// The lift's tool on this host: a yosys whose `read_slang` is built in (yosys ≥ 0.67, the
    /// pinned image) or loadable from `MUNUNU_YOSYS_SLANG_PLUGIN`. A yosys without it (a brew
    /// 0.59) SKIPS the lift tests here rather than failing them; the image runs them.
    fn yosys_available() -> bool {
        super::yosys_can_read_slang()
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

    /// mununu#981 — the seam between `read_slang --keep-hierarchy`'s snapshot and the per-module
    /// readers: a per-instance module name `<type>$<path>` reads as its type everywhere (the module
    /// map, every cell's `type`), two instances of one type collapse to one module, and yosys's own
    /// `$paramod…` names and an already-plain snapshot are left alone. The snapshot shape is the one
    /// yosys 0.68+120 writes for `producer_consumer_top` (measured in `mununu-sva`).
    #[test]
    fn a_keep_hierarchy_snapshot_reads_as_module_types() {
        let snap = r#"{"modules":{
            "producer_consumer_top":{"attributes":{"top":"00000000000000000000000000000001"},
              "ports":{},"cells":{
                "u_producer":{"type":"producer$producer_consumer_top.u_producer","connections":{"valid":[2]}},
                "u_buffer":{"type":"bounded_buffer$producer_consumer_top.u_buffer","connections":{"push":[2]}},
                "u_buffer2":{"type":"bounded_buffer$producer_consumer_top.u_buffer2","connections":{"push":[2]}}},
              "netnames":{"valid":{"bits":[2]}}},
            "producer$producer_consumer_top.u_producer":{"attributes":{},"ports":{"valid":{"direction":"output","bits":[2]}},"cells":{}},
            "bounded_buffer$producer_consumer_top.u_buffer":{"attributes":{},"ports":{"push":{"direction":"input","bits":[2]}},"cells":{}},
            "bounded_buffer$producer_consumer_top.u_buffer2":{"attributes":{},"ports":{"push":{"direction":"input","bits":[2]}},"cells":{}},
            "$paramod$abc\\fifo":{"attributes":{},"ports":{},"cells":{}}}}"#;
        let c = collapse_slang_instance_modules(snap);
        let subs = enumerate_submodules(&c, Some("producer_consumer_top"));
        assert_eq!(subs, vec!["bounded_buffer", "producer"], "{c}");
        let conns = parse_instance_connections(&c, Some("producer_consumer_top"));
        let types: Vec<(&str, &str)> = conns
            .iter()
            .map(|i| (i.instance.as_str(), i.module_type.as_str()))
            .collect();
        assert_eq!(
            types,
            vec![
                ("u_buffer", "bounded_buffer"),
                ("u_buffer2", "bounded_buffer"),
                ("u_producer", "producer")
            ]
        );
        assert_eq!(
            conns[2].port_to_net.get("valid").map(String::as_str),
            Some("valid")
        );
        let dirs = parse_port_directions_per_module(&c, None);
        assert!(dirs.contains_key("producer") && dirs.contains_key("bounded_buffer"));
        assert!(
            c.contains("$paramod$abc"),
            "yosys's own mangling is untouched: {c}"
        );
        // Negative control: the raw snapshot gives the readers instance-suffixed types, which the
        // composition driver cannot match against the per-module lifts' type names.
        let raw = parse_instance_connections(snap, Some("producer_consumer_top"));
        assert!(raw.iter().all(|i| i.module_type.contains('$')), "{raw:?}");
        // A plain snapshot (no keep-hierarchy names) comes back byte-identical.
        let plain = r#"{"modules":{"top":{"cells":{"u":{"type":"leaf"}}},"leaf":{}}}"#;
        assert_eq!(collapse_slang_instance_modules(plain), plain);
        assert_eq!(collapse_slang_instance_modules("not json"), "not json");
    }

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
    fn read_slang_hint_names_the_fix_for_each_failure_class() {
        assert_eq!(read_slang_hint("ERROR: Design elaboration failed"), "");
        let unknown = read_slang_hint("prim_count.sv:126:5: error: unknown module 'prim_flop'");
        assert!(
            unknown.contains("(* blackbox *)") && unknown.contains("blackboxed_modules"),
            "{unknown}"
        );
        let missing = read_slang_hint("ERROR: No such command: read_slang");
        assert!(missing.contains("yosys ≥ 0.67"), "{missing}");
        let both =
            read_slang_hint("No such command: read_slang\nx.sv:1:1: error: unknown module 'y'");
        assert!(
            both.contains("yosys ≥ 0.67") && both.contains("(* blackbox *)"),
            "{both}"
        );
    }

    #[test]
    fn unknown_modules_are_read_off_the_lift_error() {
        let msg = "adapter/yosys: yosys exited with status 1\nyosys log (errors):\n\
                   k.sv:310:9: error: unknown module 'prim_secded_inv_72_64_dec'\n\
                   k.sv:406:3: error: unknown module 'prim_count'\n\
                   k.sv:406:3: error: unknown module 'prim_count'\n\
                   ERROR: Design elaboration failed; see full log for details\n";
        assert_eq!(
            unknown_modules_in_error(msg),
            vec![
                "prim_count".to_string(),
                "prim_secded_inv_72_64_dec".to_string()
            ]
        );
        assert!(unknown_modules_in_error("ERROR: Design elaboration failed").is_empty());
        assert!(unknown_modules_in_error("unknown module 'bad name'").is_empty());
    }

    /// The seam `verify_auto`'s auto-stub consumes: the names the error carries feed the stub
    /// table, so a design whose FSM register is an undefined `prim_sparse_fsm_flop` gets the
    /// behavioral stub from a FAILED lift exactly as it did from a succeeded-with-cuts one.
    #[test]
    fn unknown_modules_in_the_error_reach_the_flop_stub_table() {
        let names = unknown_modules_in_error(
            "x.sv:39:3: error: unknown module 'prim_sparse_fsm_flop'\n\
             x.sv:90:3: error: unknown module 'prim_filter'",
        );
        let stubs = crate::adapter::slang::prim_stubs::stubs_for_cut_modules(&names);
        assert_eq!(
            stubs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["prim_sparse_fsm_flop.sv"]
        );
    }

    #[test]
    fn yosys_log_errors_keeps_the_diagnostics_quiet_mode_hides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("yosys.log");
        std::fs::write(
            &log,
            "-- Running command `read_slang …' --\n\
             prim_count.sv:126:5: error: unknown module 'prim_flop'\n\
             prim_count.sv:126:5: error: unknown module 'prim_flop'\n\
                 prim_flop #(\n\
                 ^\n\
             Build failed: 1 error, 0 warnings\n\
             ERROR: Design elaboration failed; see full log for details\n",
        )
        .expect("write");
        assert_eq!(
            yosys_log_errors(&log),
            "\nyosys log (errors):\nprim_count.sv:126:5: error: unknown module 'prim_flop'\n\
             ERROR: Design elaboration failed; see full log for details\n"
        );
        assert_eq!(yosys_log_errors(&dir.path().join("missing.log")), "");
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

    /// The missing-module stub's deletion sits between the hierarchy snapshot (which names the
    /// stub in `blackboxed_modules`) and `cutpoint -blackbox` (which would otherwise make it a
    /// deliberate cut); the read keeps an empty module as a black box so the stub survives to the
    /// snapshot at all.
    #[test]
    fn build_script_deletes_missing_module_stubs_after_the_snapshot() {
        use std::path::PathBuf;
        let script = slang_script(&[PathBuf::from("/tmp/foo.sv")], Some("top"), &[], &[], &[]);
        let wj = script.find("write_json").expect("write_json present");
        let del = script
            .find(&format!("delete =A:{MISSING_MODULE_ATTR};"))
            .expect("the delete present");
        let cut = script
            .find("cutpoint -blackbox")
            .expect("cutpoint -blackbox present");
        assert!(wj < del && del < cut, "{script}");
        assert!(script.contains("--empty-blackboxes"), "{script}");
    }

    #[test]
    fn build_script_injects_cutpoint_between_write_json_and_flatten() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/foo.sv")];
        let cuts = vec!["must_refresh".to_string(), "precharge_done".to_string()];
        let script = slang_script(&sources, Some("top"), &cuts, &[], &[]);
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
        let script = slang_script(&sources, Some("top"), &[], &[], &[]);
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
        let script = slang_script(&sources, Some("top"), &[], &[], &[]);
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

    // (b) source-manifest lift — caller-supplied include dirs surface as attached `-I<dir>`
    // flags on the read command (the form `read_slang` and `read_verilog` accept). Without
    // them a `` `include "frag.vh" `` that lives outside the staging tempdir fails. The
    // oracle's read lines carry them too (`test_support`).
    #[cfg(feature = "test_support")]
    #[test]
    fn oracle_read_verilog_forwards_extra_include_dirs() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/top.v")];
        let inc = vec![
            PathBuf::from("/design/include"),
            PathBuf::from("/design/rtl"),
        ];
        assert_eq!(
            oracle_read_cmds(&sources, &inc),
            vec!["read_verilog -formal -sv -I/design/include -I/design/rtl /tmp/top.v"]
        );
    }

    #[cfg(feature = "test_support")]
    #[test]
    fn oracle_chparam_passes_scope_and_quote_correctly() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/dut.sv")];
        // bare NAME → the top module; MODULE.NAME → that module; a non-integer
        // value is quoted; integers pass through.
        let params = vec![
            ("INIT_WAIT".to_string(), "4".to_string()),
            ("u_sub.DEPTH".to_string(), "8".to_string()),
            ("MODE".to_string(), "fast".to_string()),
        ];
        let script = oracle_script(&sources, Some("dut"), &[], &params);
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
        let params = vec![
            ("INIT_WAIT".to_string(), "4".to_string()),
            ("u_sub.DEPTH".to_string(), "8".to_string()),
        ];
        let script = slang_script(&sources, Some("dut"), &[], &[], &params);
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

    #[cfg(feature = "test_support")]
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

    #[cfg(feature = "test_support")]
    #[test]
    fn oracle_read_verilog_without_include_dirs_has_no_dash_i() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/tmp/top.v")];
        assert_eq!(
            oracle_read_cmds(&sources, &[]),
            vec!["read_verilog -formal -sv /tmp/top.v"]
        );
    }

    // Slang frontend also forwards the caller dirs (appended after the staging
    // tempdir include the read_slang path already adds).
    #[test]
    fn build_script_read_slang_forwards_extra_include_dirs() {
        use std::path::PathBuf;
        let sources = vec![PathBuf::from("/stage/a.sv")];
        let inc = vec![PathBuf::from("/design/pkg")];
        let script = slang_script(&sources, None, &[], &inc, &[]);
        assert!(
            script.contains(
                "read_slang --ignore-assertions --ignore-timing --empty-blackboxes -I/stage -I/design/pkg /stage/a.sv"
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
    #[ignore = "requires the mununu-sva image (yosys); run by the e2e sweep"]
    fn e2e_extra_include_dirs_resolves_fragment_without_standalone_compile() {
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
            ..Default::default()
        };
        assert!(
            sv_to_btor2(top, &no_inc).is_err(),
            "expected the lift to fail when the include dir is absent"
        );
    }

    // (a) a non-constant `while` loop — rejected by yosys `read_verilog` ("While loops are
    // only allowed in constant functions!"), the real corpus lift blocker (AssertLLM2 `xgate`)
    // — elaborates under `read_slang`. yosys + read_slang gated → the `mununu-sva` image.
    #[test]
    #[ignore = "requires the mununu-sva image (yosys + slang); run by the e2e sweep"]
    fn e2e_slang_lifts_a_non_constant_while_loop() {
        let src = "module m(input [3:0] n, output reg [7:0] y);\n\
                   integer i;\n\
                   always @* begin\n\
                     y = 8'd0; i = 0;\n\
                     while (i < n) begin y = y + 8'd1; i = i + 1; end\n\
                   end\n\
                   endmodule\n";
        let slang = YosysOptions {
            top: Some("m".to_string()),
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

    /// mununu#724 — a bodiless module nothing instantiates (an empty `module` carrying only
    /// `@mununu_guarantee` comments) is NOT reported as black-boxed: yosys marks it
    /// `(* blackbox *)`, but no cell of that type exists under the top. A marked module that
    /// IS instantiated, and an instantiated module with no definition, still are.
    #[test]
    fn blackboxed_module_names_keep_only_instantiated_modules() {
        let hier = r#"{
            "modules": {
                "top": {
                    "attributes": { "top": "00000000000000000000000000000001" },
                    "ports": {},
                    "cells": {
                        "u_x": { "type": "prim_x" },
                        "u_dff": { "type": "$dff" },
                        "u_ghost": { "type": "prim_sparse_fsm_flop" }
                    }
                },
                "prim_x": {
                    "attributes": { "blackbox": "00000000000000000000000000000001" },
                    "ports": {}
                },
                "empty_props": {
                    "attributes": { "blackbox": "00000000000000000000000000000001" },
                    "ports": {}
                }
            }
        }"#;
        assert_eq!(
            blackboxed_module_names(hier),
            vec!["prim_sparse_fsm_flop".to_string(), "prim_x".to_string()]
        );
        // The sidecar emission path still lists every marked module (contract discovery).
        assert_eq!(parse_blackbox_modules(hier).len(), 2);
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

    /// `locate_yosys` is about the binary, not about `read_slang`: a yosys without it (a brew
    /// 0.59) is still located — the lift then fails with the hint — so this probes `yosys -V`,
    /// not [`yosys_available`].
    #[test]
    fn locate_yosys_finds_binary_or_errors() {
        let res = locate_yosys();
        let binary_present = Command::new("yosys")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if binary_present {
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

    // ----- the dev-only read_verilog + sv2v oracle (`test_support`) ---------

    /// Probe whether `sv2v` is on PATH. Mirrors `yosys_available()`.
    #[cfg(feature = "test_support")]
    fn sv2v_available() -> bool {
        Command::new("sv2v")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(feature = "test_support")]
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
            frontend: SvFrontend::Verilog,
            ..Default::default()
        };
        let out = translate_sv(sv, &opts, &yopts).expect("sv2v + yosys translate");
        assert!(
            out.source_info.state_count >= 3,
            "expected ≥3 reachable states for the 3-state FSM, got {}",
            out.source_info.state_count
        );
    }

    #[cfg(feature = "test_support")]
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
            frontend: SvFrontend::Verilog,
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

    #[cfg(feature = "test_support")]
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
            frontend: SvFrontend::Verilog,
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

    #[cfg(feature = "test_support")]
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
        // Overridden for this thread only (mununu#880); the process environment is untouched.
        let _env = crate::adapter::env_var::override_for_test(
            "MUNUNU_SV2V_PATH",
            "/definitely/not/a/real/sv2v/binary",
        );
        let sv = "module empty; endmodule\n";
        let opts = AdapterOptions::default();
        let yopts = YosysOptions {
            top: Some("empty".into()),
            frontend: SvFrontend::Verilog,
            ..Default::default()
        };
        let res = translate_sv(sv, &opts, &yopts);
        let err = res.expect_err("expected sv2v spawn failure when binary path is bogus");
        assert!(
            err.message.contains("sv2v"),
            "error should name sv2v; got: {}",
            err.message
        );
    }

    #[test]
    fn parse_undefined_module_cells_finds_dangling_instance() {
        // A module that instantiates an undefined cell type (the csrng
        // prim_sparse_fsm_flop shape) — yosys leaves it as a dangling cell, not
        // a blackbox module, so it must be found via the cell scan. (The name is the
        // plain one: the sv2v `_HEX_HEX` specialisation suffix left with sv2v, 2026-11-04.)
        let hier = r#"{
          "modules": {
            "top": {
              "cells": {
                "u_state_regs": { "type": "prim_sparse_fsm_flop" },
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

#[cfg(test)]
mod cell_source_tests {
    use super::*;

    /// A netlist trimmed from the tutorial's `pump_pair` lift (yosys 0.70 + yosys-slang,
    /// 2026-10-29): ports, nets with their declaration, two `$aldff` flops on the `always_ff` at
    /// line 52 (one merged `src`), a `$mux`, and a hidden internal net.
    const FLAT: &str = r#"{"modules":{"pump_pair":{"attributes":{"top":1},
      "ports":{"clk":{"direction":"input","bits":[2]},"start_cmd":{"direction":"input","bits":[4]},
               "running":{"direction":"output","bits":[7]},"state_q":{"direction":"output","bits":[9,10]}},
      "cells":{
        "$aldff$1":{"type":"$aldff","attributes":{"src":"../rtl/pump_pair.sv:52.5"},"connections":{"Q":[9,10]}},
        "$aldff$2":{"type":"$aldff","attributes":{"src":"../rtl/pump_pair.sv:52.5|../rtl/pump_pair.sv:60.1"},"connections":{"Q":[11,12,13]}},
        "$mux$1":{"type":"$mux","attributes":{"src":"../rtl/pump_pair.sv:42.49-42.73"},"connections":{"Y":[7]}}},
      "netnames":{
        "clk":{"bits":[2],"attributes":{"src":"../rtl/pump_pair.sv:21.24"}},
        "start_cmd":{"bits":[4],"attributes":{"src":"../rtl/pump_pair.sv:23.24"}},
        "running":{"bits":[7],"attributes":{"src":"../rtl/pump_pair.sv:28.24"}},
        "state_q":{"bits":[9,10],"attributes":{"src":"../rtl/pump_pair.sv:32.24"}},
        "lof_q":{"bits":[11,12,13],"attributes":{"src":"../rtl/pump_pair.sv:36.17"}},
        "next":{"bits":[14,15],"attributes":{"src":"../rtl/pump_pair.sv:37.17"}},
        "$auto$w":{"bits":[99],"hide_name":1,"attributes":{"src":"x.sv:1.1"}}}}}}"#;

    #[test]
    fn cell_sources_reads_declarations_flops_and_ports_and_skips_wires() {
        let got: Vec<(String, String, Option<u32>, Vec<u32>)> = cell_sources(FLAT)
            .into_iter()
            .map(|c| {
                (
                    c.cell,
                    c.kind,
                    c.declared.map(|l| l.line),
                    c.assigned.iter().map(|l| l.line).collect(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("clk".into(), "input".into(), Some(21), vec![]),
                ("lof_q".into(), "register".into(), Some(36), vec![52]),
                ("running".into(), "output".into(), Some(28), vec![]),
                ("start_cmd".into(), "input".into(), Some(23), vec![]),
                ("state_q".into(), "register".into(), Some(32), vec![52]),
            ],
            "a register is a net a flop drives, its always block is the flop's src (first of a merged pair); `next` is a wire and `$auto$w` is hidden — neither is listed"
        );
        assert!(cell_sources("not json").is_empty());
    }

    /// The primary's staging name becomes the file the user named; another file is left alone.
    #[test]
    fn the_staged_primary_is_named_after_the_users_file() {
        use crate::trace::{CellSource, SourceLoc};
        let loc = |f: &str, l: u32| SourceLoc {
            file: f.into(),
            line: l,
        };
        let mut sources = vec![
            CellSource {
                cell: "state_q".into(),
                kind: "register".into(),
                declared: Some(loc("work.sv", 32)),
                assigned: vec![loc("work.sv", 52)],
            },
            CellSource {
                cell: "u_sva.seen".into(),
                kind: "register".into(),
                declared: Some(loc("pump_pair_sva.sv", 9)),
                assigned: vec![],
            },
        ];
        name_staged_primary(&mut sources, "rtl/draft1/pump_pair.sv");
        assert_eq!(
            sources[0].declared.as_ref().unwrap().to_string(),
            "pump_pair.sv:32"
        );
        assert_eq!(sources[0].assigned[0].to_string(), "pump_pair.sv:52");
        assert_eq!(
            sources[1].declared.as_ref().unwrap().to_string(),
            "pump_pair_sva.sv:9"
        );
    }
}

/// N0.2 — the lift cache at the driver: the manifest renders the plan the run executes, and
/// (in the image) a second lift of the same design is a hit with byte-identical artifacts.
#[cfg(test)]
mod lift_cache_tests {
    use super::*;

    fn yosys_id() -> ToolIdentity {
        ToolIdentity {
            path: "yosys".into(),
            identity: "Yosys 0.68 (test)".into(),
        }
    }

    /// mununu#956: a file `include`d by absolute path sits outside every keyed include dir; an
    /// edit to it must move the key, or a hit serves the model of the old file.
    #[test]
    fn an_edit_to_an_absolutely_included_file_moves_the_lift_cache_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let defs = dir.path().join("defs.svh");
        std::fs::write(&defs, "`define W 4\n").expect("write");
        let src = format!(
            "`include \"{}\"\nmodule m(input [`W-1:0] d, output [`W-1:0] q); assign q = d; endmodule\n",
            defs.display()
        );
        let yopts = YosysOptions {
            top: Some("m".into()),
            ..Default::default()
        };
        let key = || {
            LiftCacheKey::new(&lift_manifest(&src, &yopts, yosys_id()).expect("manifest"))
                .expect("key")
                .index()
                .to_string()
        };
        let before = key();
        assert_eq!(key(), before, "deterministic while nothing moved");
        std::fs::write(&defs, "`define W 8\n").expect("write");
        assert_ne!(key(), before, "the absolutely-included file is keyed");
    }

    /// mununu#956: an `include` whose argument is a macro names a file the key cannot find —
    /// the lift bypasses the cache (the manifest is refused) rather than key without it.
    #[test]
    fn an_include_the_key_cannot_resolve_bypasses_the_lift_cache() {
        let yopts = YosysOptions {
            top: Some("m".into()),
            ..Default::default()
        };
        for src in [
            "`define F \"defs.svh\"\n`include `F\nmodule m; endmodule\n",
            "`include \"../defs.svh\"\nmodule m; endmodule\n",
        ] {
            let err = lift_manifest(src, &yopts, yosys_id()).expect_err("bypass");
            assert!(err.contains("include"), "{err}");
        }
        // A mention after `//` is not a directive.
        let ok = "// see `include F for the defines\nmodule m; endmodule\n";
        assert!(lift_manifest(ok, &yopts, yosys_id()).is_ok());
    }

    /// mununu#957: yosys exits 0 but writes neither JSON snapshot — the lift is an error naming
    /// the file, not a model with the multi-clock refusal and the black-box scan switched off.
    #[cfg(unix)]
    #[test]
    fn a_lift_whose_json_snapshots_are_missing_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("yosys");
        // Writes `design.btor` (the path after the last `write_btor`) and nothing else.
        std::fs::write(
            &fake,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = -p ]; then shift; out=\"${1##*write_btor }\"; fi\n  shift\ndone\nprintf '1 sort bitvec 1\\n' > \"$out\"\n",
        )
        .expect("write fake yosys");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let _env = crate::adapter::env_var::override_for_test(
            "MUNUNU_YOSYS_PATH",
            fake.to_string_lossy().into_owned(),
        );
        let yopts = YosysOptions {
            top: Some("m".into()),
            ..Default::default()
        };
        let Err(err) = run_sv_flatten_btor2_with("module m; endmodule\n", &yopts, None) else {
            panic!("a lift with no snapshots is refused");
        };
        assert!(err.message.contains("snapshot"), "{}", err.message);
    }

    /// Without a tool on the host: the manifest of a lift carries the staged primary as
    /// `work.sv`, the exact `read_slang` script against the placeholder directory, and moves
    /// with the top, a parameter, an additional source — and not with
    /// `merge_clock_domains`, which the hit re-applies instead of keying on.
    #[test]
    fn the_manifest_renders_the_plan_the_run_executes() {
        let src =
            "module m(input clk, input d, output reg q); always @(posedge clk) q <= d; endmodule\n";
        let base = YosysOptions {
            top: Some("m".into()),
            ..Default::default()
        };
        let m = lift_manifest(src, &base, yosys_id()).expect("manifest");
        assert_eq!(m.format, LIFT_CACHE_FORMAT);
        assert_eq!(m.staged, vec![("work.sv".to_string(), src.to_string())]);
        assert_eq!(m.attempt.frontend, "slang");
        assert!(!m.attempt.cwd_in_staging_dir);
        assert!(m.attempt.plugin.is_none() && m.attempt.sv2v.is_none());
        // The staged file is rendered with the host's separator (`staging\work.sv` on Windows,
        // which yosys there accepts): the manifest renders the plan the run executes on THIS
        // host, so the expectation is built the same way, not spelled with `/`.
        let work = Path::new("/mununu-lift-cache/staging").join("work.sv");
        assert!(
            m.attempt.script.contains(&format!(
                "read_slang --ignore-assertions --ignore-timing --empty-blackboxes --top m \
                 -I/mununu-lift-cache/staging {}",
                work.display()
            )),
            "{}",
            m.attempt.script
        );
        assert!(m.attempt.script.contains("hierarchy -top m"));
        assert!(m.include_files.is_empty());
        let key = |y: &YosysOptions, s: &str| {
            LiftCacheKey::new(&lift_manifest(s, y, yosys_id()).expect("manifest"))
                .expect("key")
                .index()
                .to_string()
        };
        let k0 = key(&base, src);
        assert_eq!(key(&base, src), k0, "deterministic");
        let mut top = base.clone();
        top.top = Some("other".into());
        assert_ne!(key(&top, src), k0, "the top is in the script");
        let mut param = base.clone();
        param.params.push(("W".into(), "8".into()));
        assert_ne!(key(&param, src), k0, "a parameter is in the script");
        let mut extra = base.clone();
        extra
            .additional_sources
            .push(("peer.sv".into(), "module p; endmodule\n".into()));
        let m2 = lift_manifest(src, &extra, yosys_id()).expect("manifest");
        assert_eq!(m2.staged.len(), 2);
        assert_eq!(m2.staged[1].0, "peer.sv");
        assert_ne!(key(&extra, src), k0);
        let mut merged = base.clone();
        merged.merge_clock_domains = true;
        assert_eq!(
            key(&merged, src),
            k0,
            "the refusal is re-applied on a hit, not keyed"
        );
        assert_ne!(
            key(&base, "module m; endmodule\n"),
            k0,
            "the source is keyed"
        );
        // The port net-type strip is applied before keying, as before staging.
        let stripped = key(&base, "module m(input tri0 a); endmodule\n");
        assert_eq!(stripped, key(&base, "module m(input a); endmodule\n"));
    }

    /// An include directory's files are in the manifest: adding one is a miss.
    #[test]
    fn an_include_dir_file_is_keyed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("defs.svh"), "`define W 4\n").expect("write");
        let src = "`include \"defs.svh\"\nmodule m; endmodule\n";
        let y = YosysOptions {
            extra_include_dirs: vec![dir.path().to_path_buf()],
            ..Default::default()
        };
        let m = lift_manifest(src, &y, yosys_id()).expect("manifest");
        assert_eq!(m.include_files.len(), 1);
        assert!(m.include_files[0].0.ends_with("defs.svh"));
        let k0 = LiftCacheKey::new(&m).expect("key").index().to_string();
        std::fs::write(dir.path().join("defs.svh"), "`define W 8\n").expect("write");
        let m = lift_manifest(src, &y, yosys_id()).expect("manifest");
        assert_ne!(LiftCacheKey::new(&m).expect("key").index(), k0);
    }

    /// In the image: the first lift of a design is a miss, the second a hit with the same
    /// artifacts, a changed source a miss again, and `None` is off. Run with
    /// `cargo test -p mununu-core --lib e2e_lift_cache -- --ignored`.
    #[test]
    #[ignore = "requires yosys (the mununu-sva image)"]
    fn e2e_lift_cache_second_lift_is_a_hit_with_identical_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = LiftCache::at(dir.path().join("lift-cache"));
        let src =
            "module m(input clk, input d, output reg q); always @(posedge clk) q <= d; endmodule\n";
        let yopts = YosysOptions {
            top: Some("m".into()),
            ..Default::default()
        };
        let first = run_sv_flatten_btor2_with(src, &yopts, Some(&cache)).expect("lifts");
        assert_eq!(first.cache, LiftCacheOutcome::Miss);
        let second = run_sv_flatten_btor2_with(src, &yopts, Some(&cache)).expect("lifts");
        assert_eq!(second.cache, LiftCacheOutcome::Hit);
        assert_eq!(second.btor2, first.btor2);
        assert_eq!(second.hier_json, first.hier_json);
        assert_eq!(second.flat_netlist_json, first.flat_netlist_json);
        assert_eq!(second.clock_domains, first.clock_domains);
        assert_eq!(second.frontend_used, first.frontend_used);
        assert_eq!(second.staged_primary, first.staged_primary);
        // sv-elab writes no per-run path into the BTOR2 (`read_verilog`'s `src` comments carried
        // the staging directory, which is why this compared modulo it until 2026-11-04), so an
        // uncached lift and a hit are byte-identical; what tells them apart is `cache`.
        let changed = src.replace("q <= d", "q <= ~d");
        let third = run_sv_flatten_btor2_with(&changed, &yopts, Some(&cache)).expect("lifts");
        assert_eq!(third.cache, LiftCacheOutcome::Miss);
        assert_ne!(third.btor2, first.btor2);
        let off = run_sv_flatten_btor2_with(src, &yopts, None).expect("lifts");
        assert_eq!(off.cache, LiftCacheOutcome::Off);
        assert_eq!(off.btor2, first.btor2);
        // An unusable cache directory is a bypass, never a failure.
        std::fs::write(dir.path().join("file"), b"").expect("write");
        let blocked = LiftCache::at(dir.path().join("file"));
        let bypassed = run_sv_flatten_btor2_with(src, &yopts, Some(&blocked)).expect("lifts");
        assert!(
            matches!(bypassed.cache, LiftCacheOutcome::Miss),
            "a store failure is logged, the lift is a miss: {:?}",
            bypassed.cache
        );
    }
}
