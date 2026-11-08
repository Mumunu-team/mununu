# Installing mununu — three OSes, three tiers

> Source of truth: [`scripts/release_smoke.sh`](../scripts/release_smoke.sh) (the check every install can run), [`release.yml`](../.github/workflows/release.yml) (what the archives are and how they were tested), [`docker/README.md`](../docker/README.md) (the image) — surface: CLI+API.

mununu runs on your machines, in one of three tiers. They are measured separately and this page
says, per OS, which has been measured and how — nothing here is promised that was not run.

| tier | what you get | Linux | macOS | Windows |
|---|---|---|---|---|
| **T1 — the container** (`mununu-sva`) | everything, SystemVerilog verification included; the supported self-serve path and the one the SVA verdicts are validated in | native Docker Engine | Docker Desktop — **measured on Intel** (same verdicts as the Linux host, see below); Apple silicon runs the linux/amd64 image under emulation, **unmeasured** | Docker Desktop with the WSL2 backend — **unmeasured** |
| **T2 — the native binary** (the release archives, self-contained: `libz3` travels beside the binaries) | the engine-only tier: BTOR2 verbs, CTXDSL, synthesis, `mununu server`, `mununu licence` — no SystemVerilog lift | x86_64 and aarch64 archives, **smoke-tested on their runners with the system Z3 removed**; glibc ≥ 2.39 | Intel and Apple-silicon archives, **smoke-tested the same way**; v0.5.10's archives needed Homebrew z3 5.1 — fixed from the next release | x86_64 zip, **smoke-tested on its runner**, self-contained (`libz3.dll` inside) |
| **T3 — the native SV toolchain** | `sv verify-auto` end to end on the host: T2 + yosys with `read_slang`, slang, sv2v, Verilator, btormc, pono, cvc5 from [oss-cad-suite](https://github.com/YosysHQ/oss-cad-suite-build) | the image is this, pinned; a bare host is possible and **unmeasured** | possible, **unmeasured** | oss-cad-suite ships no Verilator for Windows, so counterexample replay needs WSL2 or T1 — **unmeasured** |

"Smoke-tested" means [`scripts/release_smoke.sh`](../scripts/release_smoke.sh) ran on that OS: the
binary starts and links Z3, `licence status` answers, a CTXDSL model is summarised, and a BTOR2
design is decided by the engine portfolio including the native (Z3) engine. "Same verdicts" means
the e2e smoke subset passed inside the image with the verdicts the native-Linux host pins.

## T2 — the native binary

Download the archive for your OS and architecture from the [GitHub release](https://github.com/Mumunu-team/mununu/releases)
(`mununu-<version>-<target>.tar.gz` / `.zip`, each with a `.sha256` beside it; the contents and
how they were built are in [`release.md`](release.md)). Each archive holds `mununu`,
`mununu-extract`, the Z3 shared library they load (`libz3.so.4` / `libz3.<v>.dylib` /
`libz3.dll`; MIT, `LICENSES/Z3-MIT.txt`), `LICENSE`, `THIRD_PARTY.md`, `LICENSES/` and
`LICENSING.md`. Nothing is installed first — keep the library beside the binaries.
(v0.5.10's Linux and macOS archives were the exception: they linked the system Z3 by install name
and need `libz3.so.4` / Homebrew z3 5.1; its release notes say so.)

### Linux (x86_64, aarch64)

```bash
tar xzf mununu-<version>-x86_64-unknown-linux-gnu.tar.gz   # glibc ≥ 2.39 (Ubuntu 24.04 or newer)
cd mununu-<version>-x86_64-unknown-linux-gnu
./mununu --version && ./mununu licence status
```

### macOS (Intel, Apple silicon)

```bash
tar xzf mununu-<version>-aarch64-apple-darwin.tar.gz   # or x86_64-apple-darwin on Intel
cd mununu-<version>-aarch64-apple-darwin
xattr -d com.apple.quarantine mununu mununu-extract    # the binaries are not notarised; Gatekeeper quarantines downloads
./mununu --version && ./mununu licence status
```

### Windows (x86_64)

Unzip `mununu-<version>-x86_64-pc-windows-msvc.zip`; `libz3.dll` is in the folder beside
`mununu.exe`, so nothing else is installed. In PowerShell:

```powershell
.\mununu.exe --version; .\mununu.exe licence status
```

SmartScreen may ask once; the binaries are not signed.

### The full check, from a checkout

The archive carries no examples. From a clone of this repository, the same smoke the release
runners passed:

```bash
scripts/release_smoke.sh /path/to/mununu /path/to/mununu-extract
```

It prints `release-smoke: ok` or says which of the four steps failed. The BTOR2 step must name
`native` among the engines that decided — that is the proof Z3 linked *and ran* on your host.

## T1 — the container

The `mununu-sva` image is the dev image plus one tag-pinned oss-cad-suite release (yosys with
`read_slang`, slang, Verilator, btormc, pono, cvc5) and sv2v; it is the only place SystemVerilog
verdicts are validated. Today it is built from the repository
([`docker/Dockerfile.sva`](../docker/Dockerfile.sva); [`docker/README.md`](../docker/README.md) has
the build and run lines); the on-prem bundle (roadmap H5) will ship it as a `docker save` tarball
with a `docker compose` file, `mununu.toml` and `mununu.lic`.

```bash
docker build -f docker/Dockerfile.sva -t mununu-sva .
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target mununu-sva make e2e      # the suite
docker run --rm -p 127.0.0.1:8080:8080 mununu-sva mununu server --addr 0.0.0.0:8080         # the API, loopback only
```

- **Linux**: native; the cgroup-aware memory ceiling (`MUNUNU_MAX_PROCESS_MEMORY_BYTES`, auto = 80 %
  of the container's limit) works out of the box.
- **macOS, Intel** (measured 2026-11-08 on an Intel laptop, Docker Desktop 29): the five-test e2e
  smoke subset passed inside the image with the same verdicts as the native-Linux host; wall times
  were within noise except one test at ≈ 2× — a laptop under Docker Desktop against a quiet Linux
  host, one run. **macOS, Apple silicon**: the published image is linux/amd64 and runs under
  emulation; the slowdown is not yet measured — expect several ×, and use T2's native arm64 binary
  for the engine-only verbs.
- **Windows**: Docker Desktop with the WSL2 backend. Unmeasured. WSL2 gives the VM 50 % of RAM by
  default; the container's cgroup limit is that VM's, so the memory ceiling fires at 80 % of it —
  raise it in `%UserProfile%\.wslconfig` (`[wsl2] memory=…`), not in mununu.

## T3 — the native SV toolchain

[`external-tools.md`](external-tools.md) lists every tool, the discovery variables
(`MUNUNU_<TOOL>_PATH`) and per-platform install lines. The short form: install the
[oss-cad-suite](https://github.com/YosysHQ/oss-cad-suite-build) release the image pins (its tag is
on the image's labels, `docker/README.md`), put its `bin/` on `PATH`, and `sv verify-auto` runs on
the host. Windows: the suite ships no Verilator, so the counterexample replay that validates an RTL
trace is unavailable there — use WSL2 or T1 for that step. None of the three bare-host T3 setups
has been measured against the image's verdicts; the policy stays: an SVA `holds` on a bare host is
presumed vacuous until reproduced in `mununu-sva`.

## Memory, licence, where things are set

- **Memory ceiling** — `MUNUNU_MAX_PROCESS_MEMORY_BYTES` ([`env-vars.md`](env-vars.md)): auto from the
  cgroup limit inside a container; on a native macOS or Windows host there is no cgroup, so the
  ceiling is off unless you set it. Past it, the remaining properties abstain with a note; nothing
  is silently dropped.
- **Licence** — `mununu licence status` reads `[licensing] file` in `mununu.toml`,
  `MUNUNU_LICENSE_FILE`, or `/etc/mununu/mununu.lic` ([`licensing.md`](licensing.md)); every state
  runs, and no state touches a verdict.
- **Deployment configuration** — `mununu.toml` via `--deployment-config`, `mununu server --config`
  or `MUNUNU_DEPLOYMENT_CONFIG` (template packs, enabled domains, the licence file).
