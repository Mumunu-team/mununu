# Releases

> Source of truth: [`release.yml`](../.github/workflows/release.yml) and [`scripts/release_smoke.sh`](../scripts/release_smoke.sh) — surface: CLI+API (the archives carry the `mununu` binary, which is both).

A release is a `v*` tag on `main`. The tag runs [`release.yml`](../.github/workflows/release.yml),
which builds `mununu` and `mununu-extract` **natively on five runners** — one per target, no
cross-compiling, because Z3 is a linked library and cross-linking it needs a foreign `libz3` nobody
has — smoke-tests each pair on its own OS, and publishes the five archives on one GitHub release.

| archive | runner | Z3 |
|---|---|---|
| `x86_64-unknown-linux-gnu.tar.gz` | `ubuntu-latest` | system (`apt-get install libz3-dev`) |
| `aarch64-unknown-linux-gnu.tar.gz` | `ubuntu-24.04-arm` | system (`apt-get install libz3-dev`) |
| `x86_64-apple-darwin.tar.gz` | `macos-15-intel` | system (`brew install z3`) |
| `aarch64-apple-darwin.tar.gz` | `macos-latest` | system (`brew install z3`) |
| `x86_64-pc-windows-msvc.zip` | `windows-latest` | the official Z3 release zip; `libz3.dll` ships in the archive |

Each archive carries `mununu`, `mununu-extract`, `LICENSE`, `THIRD_PARTY.md`, `LICENSES/` and
`LICENSING.md` (the licence-file page), plus a `.sha256` beside it. **On Linux and macOS the binary
links the system Z3**, so `libz3` must be installed before it starts (`mununu --version` fails
loudly without it); the Windows zip is self-contained. This is the engine-only tier (T2 in
[`toolchain.md`](toolchain.md)): BTOR2 verbs, CTXDSL, synthesis, `mununu server`, `mununu
licence`. SystemVerilog verification (`sv verify-auto`) needs the SV toolchain, which the
`mununu-sva` image carries ([`docker.md`](docker.md)).

## The smoke test

[`scripts/release_smoke.sh <mununu> [<mununu-extract>]`](../scripts/release_smoke.sh) runs on every
runner before packaging and on any install afterwards. It proves the binary starts and links its
libraries (`--version` loads `libz3` on every OS), the licence surface answers (`licence status`,
`missing` on a fresh host), the CTXDSL front end and the μ-calculus engines run (a summary of the
counters example), and the BTOR2 verb decides a tiny design (`twocount2c`, which reaches `bad`). A
smoke that did not run the engines would have passed on an archive carrying the wrong binary —
which is what the workflow that produced v0.4.0 did: it built the workspace root's legacy
CTXDSL-only `mununu`, not `crates/mununu-cli`.

## Dry run

`workflow_dispatch` on `release.yml` (Actions → Release → Run workflow, any branch) builds, smokes
and uploads the five archives as workflow artifacts named `mununu-dry-run-<sha>-<target>` and
creates no release. It is how a change to the workflow, the smoke or the packaging is measured
before a tag.

## Cutting one

1. Bump the version in the three `Cargo.toml`s on `main` (one PR); the `--version` line also
   carries the build identity (mununu#715).
2. `git tag vX.Y.Z && git push origin vX.Y.Z` — the tag push is the publishing act.
3. Watch the run; a red lane means no release (the `release` job needs every build), and the
   failing archive's OS is the finding.
