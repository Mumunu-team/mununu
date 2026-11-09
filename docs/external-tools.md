# External Tools

> Source of truth: [`adapter/yosys/mod.rs::locate_yosys`](../crates/mununu-core/src/adapter/yosys/mod.rs#L2552), [`plan_lift`](../crates/mununu-core/src/adapter/yosys/mod.rs#L639), [`slang_plugin_arg`](../crates/mununu-core/src/adapter/yosys/mod.rs#L2600), [`adapter/cvc5/mod.rs::locate_cvc5`](../crates/mununu-core/src/adapter/cvc5/mod.rs#L78) — surface: CLI+API

Mununu's verification engine, model checker, and synthesiser are pure Rust + the linked [Z3](https://github.com/Z3Prover/z3) library. Several pipelines also invoke external command-line tools via subprocess. Those tools are **optional** — mununu functions without them, and individual pipelines emit a structured `AdapterWarning` or `AdapterError` when invoked while a required tool is missing, never silently fail.

This document is the single canonical place describing each optional tool: what it does for mununu, how to install it on each supported platform, and which env var overrides the discovery path.

---

## Quick summary

| Tool | Required for | Install (macOS) | Install (Debian/Ubuntu) | Env var override |
|---|---|---|---|---|
| [Z3](#z3-required-linked-library) | Everything (linked at build time) | `brew install z3` | `apt install libz3-dev` | `LIBRARY_PATH` |
| [sv2v](#sv2v--dev-only-differential-oracle) | **dev-only**: the `read_verilog` + sv2v differential oracle of `test_support` builds — never the product lift | `brew install zachjs-sv2v` | Build from [zachjs/sv2v](https://github.com/zachjs/sv2v) | `MUNUNU_SV2V_PATH` (dev-only) |
| [Yosys](#yosys) | SystemVerilog → BTOR2 lift via yosys-slang's `read_slang` (built into yosys ≥ 0.67; `MUNUNU_YOSYS_SLANG_PLUGIN` loads a plugin on an older yosys) | `brew install yosys` | `apt install yosys` | `MUNUNU_YOSYS_PATH` |
| [SymbiYosys (sby)](#sby--symbiyosys) | Verification oracle on RTL fixtures | Bundled with Yosys via OSS-CAD-Suite | Bundled with Yosys | `MUNUNU_SBY_PATH` |
| [CVC5](#cvc5) | Craig interpolation predicate source (`--predicate-source craig`) | `brew install cvc5` | `apt install cvc5` (Debian 12+ / Ubuntu 24.04+) | `MUNUNU_CVC5_PATH` |
| [Verilator](#verilator) | Reset-state simulation seeding (R-S2b, sidecar `simulate_reset`) | `brew install verilator` | `apt install verilator` | `MUNUNU_VERILATOR_PATH` |
| [slang](#slang) | SVA front-end (`sv extract-sva` / `POST /api/v1/sv/extract-sva`) | release tarball / build from source | release tarball / build from source | `MUNUNU_SLANG_PATH` |
| [circt-verilog](#circt-verilog) | Alternative extraction frontend (`mununu-extract circt`) | build CIRCT / release tarball | build CIRCT / release tarball | n/a (pipe input) |

Every **discovered** tool above (all but Z3, which is linked, and circt-verilog, which is a pipe-input producer) follows the same discovery pattern: the `MUNUNU_<TOOL>_PATH` env var is checked first; if absent, the bare binary name is invoked via `$PATH`. On a per-process basis — mununu does not cache discovery results across CLI invocations.

**Licensing posture (see [`deny.toml`](../deny.toml) + the `license-check` CI job).** Z3 is linked in-process (MIT — permissive, no contamination). Every other tool here is invoked as a **subprocess** (or, for circt-verilog, consumed as pipe input) — i.e. "mere aggregation", so its license (even Verilator's LGPL-3/Artistic-2) does **not** contaminate mununu's source license. None are bundled into the release archives; contributors install them. The on-prem bundle image (`scripts/bundle.sh`) is the exception: it carries one pinned oss-cad-suite release unmodified, with the suite's own licence texts, as listed in [`THIRD_PARTY.md`](../THIRD_PARTY.md) § "Bundled in the on-prem image". The only license-contamination vector — the *linked* Cargo crate graph — is gated by `cargo deny check licenses` (deny-by-default permissive allow-list).

---

## Z3 (required, linked library)

> Source of truth: [`crates/mununu-core/Cargo.toml`](../crates/mununu-core/Cargo.toml#L101) — `z3 = "0.21"` — surface: build-time

Z3 is mununu's primary SMT backend, linked in-process via the `z3 = "0.21"` Rust crate. Unlike the other tools listed below, Z3 is **mandatory** — the workspace will not build without it.

### macOS
```bash
brew install z3
# The Rust crate's build script needs LIBRARY_PATH set:
export LIBRARY_PATH=/usr/local/opt/z3/lib   # Intel
# or
export LIBRARY_PATH=/opt/homebrew/opt/z3/lib  # Apple Silicon
```

### Debian / Ubuntu
```bash
apt install libz3-dev
```

### Windows (x86_64)

The release archive needs nothing: `libz3.dll` ships beside `mununu.exe` ([`docs/install.md`](install.md)).
To **build** mununu on Windows, use the official Z3 release zip — the one the OS test matrix
(`.github/workflows/os-matrix.yml`, roadmap I1) installs — not a Cargo feature: `z3-sys` 0.13's
`vendored`, `vcpkg` and `gh-release` features are mutually exclusive in its build script, so a
mununu-core feature for any of them breaks every `--all-features` build. In PowerShell:

```powershell
# https://github.com/Z3Prover/z3/releases — the x64-win zip matching docs/toolchain.md § "Z3 on each OS"
Expand-Archive z3-*-x64-win.zip -DestinationPath C:\z3
Copy-Item C:\z3\z3-*\bin\libz3.lib C:\z3\z3-*\bin\z3.lib       # the crate links `z3`, the zip ships `libz3`
$env:Z3_LIBRARY_PATH_OVERRIDE = (Resolve-Path C:\z3\z3-*\bin)  # where z3-sys finds the library
$env:PATH = "$env:Z3_LIBRARY_PATH_OVERRIDE;$env:PATH"           # z3.exe for the version probe, libz3.dll at run time
cargo build --release -p mununu-cli
```

Measured: the matrix's Windows lane builds and runs the unit tests this way (3 743 of 3 746; the
three absent are the Unix-only process-group tests, mununu#904). Unmeasured: a Windows **user**
host — the maintainers have none yet.

The `docker/Dockerfile.dev` image installs `libz3-dev` automatically — from **Debian trixie**
(libz3 **4.13.3**) since 2026-09-22. That is deliberate: `z3-sys` 0.13 (the crate behind
`z3 = "0.21"`) declares Z3 ≥ 4.13.3 as its minimum and detects the linked version at build
time. Bookworm's 4.8.12 does still compile and link (verified before the base bump), because the
crate only warns on the mismatch and mununu calls no version-gated API — but a host on 4.8.x is
building against a library older than the crate's stated floor. Prefer ≥ 4.13.3.

### Build-from-source
See the [upstream Z3 docs](https://github.com/Z3Prover/z3#building-z3-from-source-using-cmake).

---

## sv2v — dev-only differential oracle

> Concept: the dev-only differential oracle of `test_support` builds (`SvFrontend::Verilog` in `crates/mununu-core/src/adapter/yosys/mod.rs`, 2026-11-04) — no CLI, API or UI surface reaches it, so it carries no anchor.

sv2v lowers SystemVerilog-2017 constructs (interfaces, structs, `always_ff`, generates, parameterised modules) to a Verilog-2005 subset that yosys's native `read_verilog` can ingest. **The product does not use it** (N1, 2026-11-04): the SV → BTOR2 lift is yosys-slang's `read_slang` only. The `--frontend` flag, the `--preprocessor sv2v` flag, the `sv preprocess` subcommand, the API fields `lift.frontend` / `use_sv2v`, the `verify.toml` key `options.use_sv2v` (now refused by config validation) and the env vars `MUNUNU_YOSYS_FRONTEND` / `MUNUNU_USE_SV2V` are gone.

What survives is a **developer oracle** in `test_support` builds (`cargo test --features test_support`, the `mununu-sva` image's e2e suite): `SvFrontend::Verilog` lifts a source through `read_verilog` after sv2v, so a test can lift one design twice and compare the two models' verdicts (the partial-write agreement test, the minted-expression elaboration oracle). A product build refuses `SvFrontend::Verilog` with a reason; nothing on the CLI or the HTTP API can select it. Note that sv2v silently drops `assert property`, so the oracle's lift carries no SVA — this is why it was retired as a product path (`docs/policies/ci-and-hooks.md`).

### Install (only to run the oracle tests)
macOS: `brew install zachjs-sv2v`. Elsewhere: build from [zachjs/sv2v](https://github.com/zachjs/sv2v) (`make`, then put `bin/sv2v` on `$PATH`). The `mununu-sva` image pins sv2v `v0.0.13` for this purpose only.

### Discovery (test_support builds only)
1. `MUNUNU_SV2V_PATH` env var (explicit override).
2. `sv2v` on `$PATH`.

Missing-tool behaviour: an oracle test's lift errors with an `UnsupportedConstruct` adapter error naming the install instructions ("the dev-only read_verilog oracle needs it"). No product path is affected.

---

## Yosys

> Source of truth: [`adapter/yosys/mod.rs::locate_yosys`](../crates/mununu-core/src/adapter/yosys/mod.rs#L2552) + [`slang_plugin_arg`](../crates/mununu-core/src/adapter/yosys/mod.rs#L2600) — surface: CLI+API

Yosys is the open-source synthesis suite mununu uses to lower SystemVerilog into BTOR2 — the word-level intermediate representation the KMTS lifter consumes. The RTL front end is yosys-slang's `read_slang` (N1, 2026-11-04: the only one), **built into yosys ≥ 0.67**; on an older yosys set `MUNUNU_YOSYS_SLANG_PLUGIN` to a yosys-slang plugin (loaded with `-m`). A yosys with neither (Homebrew's 0.59, Debian's 0.23) lifts **no** SystemVerilog: the error names the fix.

Used by:
- The SystemVerilog → BTOR2 pipeline (`adapter/yosys/`): `read_slang --ignore-assertions` (SVA is extracted separately by slang), `hierarchy`, `flatten`, `write_btor`.
- Per-submodule emission (`write_btor <m>.btor`) without `flatten`, preserving module hierarchy.

### macOS
```bash
brew install yosys
# Yosys version: yosys --version
```

### Debian / Ubuntu
```bash
apt install yosys
```

### Windows (x86_64) — the native SV toolchain, T3

No Homebrew or apt: take the OSS-CAD-Suite **windows-x64** bundle (below; the same tag the
`mununu-sva` image pins, `2026-08-24`), unzip it, and point mununu at its binaries — the paths
take backslashes as Windows gives them:

```powershell
Expand-Archive oss-cad-suite-windows-x64-*.zip -DestinationPath C:\oss-cad-suite
$env:MUNUNU_YOSYS_PATH = "C:\oss-cad-suite\oss-cad-suite\bin\yosys.exe"
$env:MUNUNU_SLANG_PATH = "C:\oss-cad-suite\oss-cad-suite\bin\slang.exe"
.\mununu.exe sv verify-auto design.sv --source design_sva.sv --top design --json
```

What the Windows bundle does **not** carry: Verilator. The counterexample replay
(`--emit-testbench`) and the reset-state simulation seeding therefore have no native path on
Windows; use WSL2 or the container (T1) for those steps — [`docs/install.md`](install.md) § T3
says the same per tier. **Unmeasured** (roadmap I4): no Windows host has run the SV fragment's
truth table natively yet; when one does, the measurement goes into `docs/install.md`'s tier
table, and a stage Windows cannot run must end in the structured adapter error the failure-path
policy promises, never a hang — that check is part of the same measurement.

### OSS-CAD-Suite (cross-platform, includes sby)
The [OSS-CAD-Suite](https://github.com/YosysHQ/oss-cad-suite-build) ships Yosys + sby + many other YosysHQ tools as a self-contained release tarball. Recommended when you need both Yosys and sby together.

It is also what the `mununu-sva` image (`docker/Dockerfile.sva`) carries — one tag-pinned release (`2026-08-24` → Yosys 0.68, the yosys-slang `slang.so` plugin, slang 11.0, Verilator 5.051, btormc, pono, cvc5), the same tag the sibling rosf and monono images pin, asserted at build time and readable off the image labels (`docker/README.md`). The suite does **not** ship sv2v; the image adds it separately, for the dev-only differential oracle only (`test_support` builds).

### Discovery
1. `MUNUNU_YOSYS_PATH` env var (explicit override).
2. `yosys` on `$PATH`.

Missing-tool behaviour: the SV pipeline errors with an `UnsupportedConstruct` adapter error; a yosys that has no `read_slang` fails the lift with the error tail "this yosys has no `read_slang`: use yosys ≥ 0.67 … or set MUNUNU_YOSYS_SLANG_PLUGIN". Other mununu pipelines (CTXDSL, agentic, microcode) are unaffected.

---

## sby / SymbiYosys

> Source of truth: validation milestones M.0–M.4 — surface: contributor

[SymbiYosys](https://symbiyosys.readthedocs.io/) is the verification oracle mununu uses to cross-check verdicts on RTL fixtures (the M.0–M.4 milestones). Not invoked by any user-facing CLI command today — only by the milestone validation harness.

### Bundled installs
sby ships with the OSS-CAD-Suite tarball or via the `yosys-bin` Homebrew formula on macOS.

### macOS
```bash
brew install --HEAD symbiyosys
# or use the OSS-CAD-Suite tarball
```

### Debian / Ubuntu
```bash
apt install yosys symbiyosys
```

### Discovery
1. `MUNUNU_SBY_PATH` env var (explicit override).
2. `sby` on `$PATH`.

Missing-tool behaviour: the milestone validation harness skips fixtures that require an SBY oracle; mununu's other pipelines are unaffected.

---

## CVC5

> Source of truth: [`adapter/cvc5/mod.rs::locate_cvc5`](../crates/mununu-core/src/adapter/cvc5/mod.rs#L78) — surface: CLI+API

[CVC5](https://cvc5.github.io/) is an open-source SMT solver developed at Stanford + The University of Iowa, with strong support for SMT-LIB Craig interpolation via `(get-interpolant ...)`. Mununu uses it as a **second SMT backend** (alongside Z3) for the `PredicateSource::CraigInterpolation` path in the R.5 CEGAR loop.

CVC5 is invoked via subprocess (mirroring the sv2v / Yosys / SBY pattern); mununu does not link the `cvc5` / `cvc5-sys` Rust crates because they carry heavy build dep chains (GMP, CLN, antlr3-c).

Used by:
- The R.5 CEGAR loop when the predicate source is `Craig` (CLI: `--predicate-source craig`; sidecar: `cegar.predicate_source = "craig"`).
- Future Craig-based refinement paths (mununu's roadmap may extend CVC5 use to other refinement strategies).

### macOS
```bash
brew install cvc5
# Verify install: cvc5 --version
```

### Debian 12+ / Ubuntu 24.04+
```bash
apt install cvc5
```

### Older Debian / Ubuntu / others
The [cvc5 GitHub releases](https://github.com/cvc5/cvc5/releases) page provides pre-built Linux x86_64, macOS arm64+x86_64, and Windows binaries. Download the tarball, extract, and either copy `cvc5` to `/usr/local/bin/` or set `MUNUNU_CVC5_PATH` to the extracted binary.

### Build from source
```bash
git clone https://github.com/cvc5/cvc5
cd cvc5
./configure.sh --auto-download
cd build && make -j
# The produced binary at build/bin/cvc5; copy to $PATH or set MUNUNU_CVC5_PATH.
```

### Discovery
1. `MUNUNU_CVC5_PATH` env var (explicit override).
2. `cvc5` on `$PATH`.

### Missing-tool behaviour

When `PredicateSource::CraigInterpolation` is selected (via CLI or sidecar) but CVC5 isn't found at runtime, mununu emits an `AdapterWarning` documenting the missing dep + the install instructions, then **falls back to the WeakestPrecondition heuristic** for the CEGAR run. The verification verdict is still computed; it just uses a less precise predicate-discovery mechanism. Other mununu pipelines that don't request Craig are unaffected.

### Version requirements

CVC5 ≥ 1.0 is required for the `(set-option :produce-interpolants true)` + `(get-interpolant ...)` syntax mununu uses. The Homebrew / apt packages ship versions ≥ 1.0 by default.

### Pattern B (deployment)

CVC5 is **not** installed by `docker/Dockerfile.dev`. Contributors install it locally per the instructions above. This mirrors the discipline already in place for sv2v / Yosys / SBY (all subprocess-only, all contributor-installed). The trade-off:

- Pro: CI doesn't pay the CVC5 build/install cost (~50MB image growth).
- Pro: Mininum Docker image stays small.
- Con: CI doesn't exercise the Craig path; the `#[ignore]`d integration tests stay ignored on CI. Contributors run them locally via `cargo test -- --ignored` after `brew install cvc5`.

When the Craig path becomes load-bearing for a milestone (e.g. V.3 speculative non-interference), a pattern-A follow-up will add CVC5 to `Dockerfile.dev` + remove the `#[ignore]`s. Tracked as sub-item 3.6 in the R-track multi-session breakdown — a deferred-with-trigger entry that fires when mununu opens a binary distribution channel (Homebrew formula / Debian package / Docker Hub image).

---

## Verilator

> Source of truth: [`adapter/verilator/mod.rs::locate_verilator`](../crates/mununu-core/src/adapter/verilator/mod.rs#L79) — surface: CLI+API

[Verilator](https://www.veripool.org/verilator/) is an open-source SystemVerilog
simulator. Mununu invokes it via subprocess for **R-S2b reset-state simulation
seeding**: when a sidecar declares `simulate_reset`, mununu compiles + runs a short
concrete simulation through Verilator, captures the post-reset valuation of the
declared `observe_registers`, and feeds it to the bit-blaster's predicate seeder.

Verilator's own license (LGPL-3.0 / Artistic-2.0) does not affect mununu: it is run
as a separate process (mere aggregation) and is never bundled.

### macOS
```bash
brew install verilator
# Verify install: verilator --version
```

### Debian / Ubuntu
```bash
apt install verilator
```

### Windows
The OSS-CAD-Suite windows-x64 bundle ships no Verilator, and there is no supported native build
for mununu's use on Windows. Anything that needs it — the reset-state simulation seeding here and
the `--emit-testbench` counterexample replay — runs in WSL2 or in the `mununu-sva` container
(`docs/install.md` § T1 / T3). Without it the verdicts are still computed; the replay and the
seeding are absent and said so (the missing-tool behaviour below). Unmeasured on a Windows host
(roadmap I4).

### Discovery
1. `MUNUNU_VERILATOR_PATH` env var (explicit override).
2. `verilator` on `$PATH`.

### Missing-tool behaviour
When a sidecar requests `simulate_reset` but Verilator isn't found, mununu falls back
gracefully to the other Phase-9 seeding strategies (BTOR2 `init` lines, etc.) — the
verdict is still computed, just without the simulation-derived seeds.

---

## circt-verilog

> Source of truth: [`mununu-extract/src/circt.rs`](../crates/mununu-extract/src/circt.rs) — surface: CLI (`mununu-extract circt`)

[`circt-verilog`](https://github.com/llvm/circt) (the CIRCT project's slang-based
Verilog frontend) lowers SystemVerilog to CIRCT MLIR. Unlike the discovered tools
above, mununu does **not** spawn it — it is a **pipe-input producer**: the user runs
circt-verilog and pipes its MLIR into the `mununu-extract circt` subcommand, which
parses the `hw`/`comb`/`seq` dialects into an explicit-state Kripke structure.

```bash
circt-verilog design.sv | mununu-extract circt --output spec.espec.json
```

This is an **alternative** extraction frontend (the primary RTL path is
yosys-slang `read_slang` → Yosys → BTOR2). Because it's pipe input, there is no `MUNUNU_*_PATH`
discovery and no in-mununu version check — the user supplies the MLIR. CIRCT is
Apache-2.0 (with the LLVM exception); as upstream tooling whose output mununu
consumes, it imposes no obligation on mununu.

---

## slang

> Source of truth: [`adapter/slang/mod.rs::locate_slang`](../crates/mununu-core/src/adapter/slang/mod.rs#L48) — surface: CLI+API

[slang](https://github.com/MikePopoloski/slang) is an open-source, full-featured
SystemVerilog-2017 compiler front-end (MIT-licensed). Mununu uses it as a **discovered
subprocess** (the cvc5 / Yosys pattern) to parse **SystemVerilog Assertions** out
of RTL: `slang --ast-json` serialises the elaborated design — including `assert` /
`assume` / `cover property` — as a structured AST, which mununu's Tier-1/Tier-2 translator
([`adapter/slang/translate.rs`](../crates/mununu-core/src/adapter/slang/translate.rs))
lowers to mu-calculus formulas the verifier can check.

slang is the right choice (the roadmap XL.0 decision) over circt-verilog because it runs a
**real SV preprocessor**: OpenTitan's `` `ASSERT `` macros and `` `include `` directives
expand correctly, which tree-sitter (no preprocessor) and sv2v (drops concurrent
assertions) cannot do.

Used by:
- `mununu sv extract-sva` (CLI) and `POST /api/v1/sv/extract-sva` (API) — the SVA
  front-end: extract + translate a design's assertions to mu-calculus (Tier-1 boolean /
  `|->` / `|=>` / `disable iff` / `cover`, the XL.1c reductions, the XL.2 `EF`→`AG EF`
  recoverability companion, and the XL.3 Tier-2 history `$past`/`$stable`/`$rose`/`$fell`).

### macOS / Linux
The [slang releases](https://github.com/MikePopoloski/slang/releases) page ships pre-built
Linux x86_64 and macOS arm64 binaries. Download, extract, and either copy `slang` to
`/usr/local/bin/` or set `MUNUNU_SLANG_PATH` to the extracted binary.

The oss-cad-suite tarball (see the Yosys section) bundles a `slang` build too, alongside the
`slang.so` yosys plugin built from the same snapshot; the `mununu-sva` image uses that one, so the
SVA extractor and the `read_slang` RTL lift never disagree about the language front-end.

### Build from source
```bash
git clone https://github.com/MikePopoloski/slang
cd slang
cmake -B build && cmake --build build --target slang_driver -j
# The produced binary at build/bin/slang; copy to $PATH or set MUNUNU_SLANG_PATH.
```

### Discovery
1. `MUNUNU_SLANG_PATH` env var (explicit override).
2. `slang` on `$PATH`.

A successful probe parses the version from `slang --version` (diagnostic only; mununu does
not gate on the version).

### Missing-tool behaviour

When SVA extraction is requested but slang isn't found at runtime, mununu returns a
structured error naming the missing tool + the install instructions. The SVA-extraction
feature degrades gracefully — model verification and mununu-annotation properties are
unaffected (they don't need slang). This is the cvc5 precedent.

### Pattern B (deployment)

slang is **not** bundled in mununu artifacts or installed by `docker/Dockerfile.dev`;
contributors install it locally per the above, and CI uses the Linux prebuilt. This
mirrors the subprocess discipline for sv2v / Yosys / SBY / CVC5 — all contributor-installed,
none linked. As a subprocess whose output mununu consumes, slang's MIT license imposes no
obligation on mununu ("mere aggregation"; see the licensing posture above).

---

## Verifying your install

After installing any of these tools, you can confirm mununu discovers them via:

```bash
# Z3 (linked; check via cargo)
cargo run -p mununu-cli -- --version

# sv2v (dev-only differential oracle; not needed for the product lift)
sv2v --version

# Yosys
yosys -V

# sby
sby --help

# CVC5
cvc5 --version

# Verilator
verilator --version
```

Mununu's discovery layer parses the version strings (or just confirms exit-zero) — there's no semver gating today, but each adapter's tests are pinned to versions known to work. If you hit a "binary not found" error from a mununu CLI subcommand, set the corresponding `MUNUNU_*_PATH` env var to the absolute path of the binary, or ensure it's on `$PATH`.

---

## See also

- [`docs/dev-container.md`](dev-container.md) — pinned Docker dev container for reproducible CI-style builds.
- [`docs/build-recipes.md`](build-recipes.md) — finer-grained `cargo` invocations for per-crate work.
- [`docs/cli-cookbook.md`](cli-cookbook.md) — common `mununu` CLI invocations, including `--predicate-source craig`.
