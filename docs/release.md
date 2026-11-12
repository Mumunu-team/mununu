# Releases

> Source of truth: [`release.yml`](../.github/workflows/release.yml) and [`scripts/release_smoke.sh`](../scripts/release_smoke.sh) — surface: CLI+API (the archives carry the `mununu` binary, which is both).

A release is a `v*` tag on `main`. The tag runs [`release.yml`](../.github/workflows/release.yml),
which builds `mununu` and `mununu-extract` **natively on five runners** — one per target, no
cross-compiling, because Z3 is a linked library and cross-linking it needs a foreign `libz3` nobody
has — smoke-tests each pair on its own OS, and publishes the five archives on one GitHub release.

| archive | runner | Z3 in the archive |
|---|---|---|
| `x86_64-unknown-linux-gnu.tar.gz` | `ubuntu-latest` | `libz3.so.4` from the runner's apt package, loaded through `$ORIGIN` (patchelf) |
| `aarch64-unknown-linux-gnu.tar.gz` | `ubuntu-24.04-arm` | same |
| `x86_64-apple-darwin.tar.gz` | `macos-15-intel` | `libz3.<v>.dylib` from the runner's Homebrew, loaded through `@executable_path` (install_name_tool, ad-hoc re-signed) |
| `aarch64-apple-darwin.tar.gz` | `macos-latest` | same |
| `x86_64-pc-windows-msvc.zip` | `windows-latest` | `libz3.dll` from the official Z3 release zip |

Each archive carries `mununu`, `mununu-extract`, the Z3 shared library beside them (MIT,
`LICENSES/Z3-MIT.txt`), `LICENSE`, `THIRD_PARTY.md`, `LICENSES/` and `LICENSING.md` (the
licence-file page), plus a `.sha256` beside it. **The archives are self-contained**: nothing is
installed first. The workflow proves it on every runner by removing the runner's own Z3 before
the packaged binary runs the smoke (v0.5.10 linked the runner's Z3 by install name and failed to
start on a host with another version — found on a real host after publishing; its release notes
say so). Linux needs glibc ≥ 2.39 (built on Ubuntu 24.04). This is the engine-only tier (T2 in
[`toolchain.md`](toolchain.md)): BTOR2 verbs, CTXDSL, synthesis, `mununu server`, `mununu
licence`. SystemVerilog verification (`sv verify-auto`) needs the SV toolchain, which the
`mununu-sva` image carries ([`docker.md`](docker.md)).

## The smoke test

> Source of truth: [`scripts/release_smoke.sh`](../scripts/release_smoke.sh) — surface: CLI+API

[`scripts/release_smoke.sh <mununu> [<mununu-extract>]`](../scripts/release_smoke.sh) runs on every
runner against the **packaged** binaries, after the runner's Z3 is removed, and on any install
afterwards. It proves the binary starts and links its
libraries (`--version` loads `libz3` on every OS), the licence surface answers (`licence status`,
`missing` on a fresh host), the CTXDSL front end and the μ-calculus engines run (a summary of the
counters example), and the BTOR2 verb decides a tiny design (`twocount2c`, which reaches `bad`). A
smoke that did not run the engines would have passed on an archive carrying the wrong binary —
which is what the workflow that produced v0.4.0 did: it built the workspace root's legacy
CTXDSL-only `mununu`, not `crates/mununu-cli`.

## Dry run

> Source of truth: [`release.yml`](../.github/workflows/release.yml#L19) (`workflow_dispatch`) — surface: CLI+API

`workflow_dispatch` on `release.yml` (Actions → Release → Run workflow, any branch) builds, smokes
and uploads the five archives as workflow artifacts named `mununu-dry-run-<sha>-<target>` and
creates no release. It is how a change to the workflow, the smoke or the packaging is measured
before a tag.

## Cutting one

> Source of truth: [`release.yml`](../.github/workflows/release.yml#L17) (the `v*` tag trigger) — surface: CLI+API

0. Run the review pass's policy gates on `main` since the last tag (the `review-orchestrator`
   agent under `.claude/agents/`, tier A): a consumer briefing for every surface change, the
   API-schema drift settled, the private-name and absolute-path scans clean, a `Repro:` trailer
   on every fix. A red gate is a finding to fix before the tag, not a formality to note.
1. Bump the version in the three `Cargo.toml`s on `main` (one PR); the `--version` line also
   carries the build identity (mununu#715).
2. `git tag vX.Y.Z && git push origin vX.Y.Z` — the tag push is the publishing act.
3. Watch the run; a red lane means no release (the `release` job needs every build), and the
   failing archive's OS is the finding.
