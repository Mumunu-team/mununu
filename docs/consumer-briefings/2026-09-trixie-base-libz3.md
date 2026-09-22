# Consumer briefing — `mununu-dev` (and therefore `mununu-sva`) moves from Debian bookworm to trixie, for libz3

> **Audience:** **monono** (its formal lane runs inside `mununu-sva` via `make formal-docker`;
> that image is rebuilt on a different Debian base); **rosf** (no code impact — its images do not
> link z3; read the base-image note and the table); **hw-verification-uba** and **mununu-ui**
> (no impact — read the table and stop).

## TL;DR

`docker/Dockerfile.dev` now starts `FROM rust:1.95-slim-trixie` instead of `-slim-bookworm`. The
only reason is **libz3**: mununu#568 moved the `z3` crate to 0.21 (RUSTSEC-2026-0295), whose
`z3-sys` 0.13 declares **Z3 ≥ 4.13.3** as its minimum and now detects the linked version at build
time. Bookworm ships libz3 **4.8.12**; trixie ships **4.13.3**. The Rust toolchain pin (1.95), every
other package, the cargo volume layout and every `docker run` line are unchanged.

**No engine code changed and no verdict changes.** The 4.8.12 combination was verified to compile
and link before this bump (z3-sys only warns on the mismatch; nothing mununu calls is
version-gated), so this is alignment, not a bug fix: the image's library now meets the crate's
stated floor instead of sitting below it under a warning.

**Action for monono:** rebuild `mununu-sva` when you next rebuild anyway (one command, below).
Nothing on your side changes until you do; nothing breaks if you don't.
**Action for rosf / hw-verif / mununu-ui:** none.

## What changed at engine level

Nothing. `crates/`, `Cargo.toml` and `Cargo.lock` are untouched by this PR. The change is one
`FROM` line in `docker/Dockerfile.dev` (with its rationale in a comment), plus the docs that
describe the image (`docs/dev-container.md`, `docs/external-tools.md`, `docs/docker.md`).

What changed *under* the engine, in the image:

| | bookworm (before) | trixie (after) |
|---|---|---|
| Debian | 12 | 13 |
| libz3 (`libz3-dev`, linked by the `z3` crate) | 4.8.12-3.1 | 4.13.3-1 |
| glibc | 2.36 | 2.41 |
| python3 (helper scripts) | 3.11 | 3.13 |
| Rust toolchain | 1.95 (pinned, unchanged) | 1.95 |
| `cargo-nextest`, rustfmt, clippy, gdb, cmake, libssl-dev | present | present (same package names) |

The oss-cad-suite tarball `mununu-sva` streams on top is a self-contained prebuilt release with its
own `lib/` tree and rpaths; it does not depend on the Debian release beneath it.

## The measurement

<!-- VALIDATION -->
Measured on the host, 2026-09-22, before opening the PR:

1. **Before the bump — bookworm, libz3 4.8.12, z3 0.21 / z3-sys 0.13:** `cargo check -p
   mununu-core --features api` inside the old `mununu-dev` compiles and links, 2m58s, exit 0.
   (So the pre-bump image was not broken; this PR is alignment.)
2. **After — `docker build -f docker/Dockerfile.dev -t mununu-dev:trixie .`:** builds, 2.31 GB
   (bookworm image: 2.28 GB). Inside it: `cat /etc/debian_version` → `13.5`; `dpkg -s libz3-dev` →
   `4.13.3-1`; `rustc 1.95.0`; `cargo-nextest 0.9.146`; `cargo check -p mununu-core --features api`
   → `Finished`, 1m15s, exit 0.
3. **`mununu-sva` on the new base:** `docker build -f docker/Dockerfile.sva -t mununu-sva:trixie .`
   builds in 304 s (the suite layer re-downloaded, as expected when `FROM` changes) with every
   build-time version assertion passing; labels unchanged (`oss-cad-suite.tag=2026-08-24`,
   `yosys.version=0.68`, `slang.version=11.0`, `sv2v.version=v0.0.13`,
   `verilator.version=5.051`). Inside it: Debian 13.5, libz3 4.13.3-1, `Yosys 0.68+120`, `slang
   11.0.448`. The reproducible slang-gated test ran there —
   `cargo test -p mununu-core --lib --all-features e2e_csrng_real_sva_verdict_breakdown -- --ignored`
   — and decided the real OpenTitan `csrng_main_sm` SVA exactly as before: **2 translated, 0
   unsupported, HOLDS=2 VIOLATED=0 UNKNOWN=0 SKIPPED=0**, 9.40 s. Only that one ignored test was
   run locally; the full `make e2e` sweep is the nightly's job (`e2e.yml`), on the same
   Dockerfiles.
4. **CI:** the PR's `lint-and-test` job builds the image from this Dockerfile and runs the full
   `make ci` in it — that is the workspace-wide validation; the nightly `e2e.yml` builds
   `mununu-sva` on top and runs `make e2e`.
<!-- /VALIDATION -->

## Per-consumer

### monono

- **What to update on your side:** nothing in the tree. `tools/versions.lock` pins the
  oss-cad-suite tag (2026-08-24), which is unchanged; the image labels are unchanged.
- **What to expect:** the same verdicts from the same engine. `make formal-docker` behaves the
  same; the container is Debian 13 underneath, which only matters if a script of yours assumes
  bookworm's python3 (3.11 → 3.13) — none of the ones in the handoff do.
- **Report parsing impact:** none (no wire-format change).
- **Docker rebuild disposition:** `mununu-sva` — *only if the dev workflow requires the new
  binary*; the rebuilt image is not required for correctness, it is the one whose libz3 meets the
  crate's floor. `monono-dev` — no (it does not link z3).
- **Test the transition:**

  ```bash
  cd ../mununu
  docker build -f docker/Dockerfile.dev -t mununu-dev .          # trixie base
  docker build -f docker/Dockerfile.sva -t mununu-sva .          # inherits it
  docker run --rm mununu-sva sh -c 'cat /etc/debian_version; dpkg -s libz3-dev | grep ^Version'
  # expect: 13.x and 4.13.3-1
  cd ../monono && make formal-docker                              # same verdicts as before
  ```

### rosf

- **What to update:** nothing. `rosf-dev`, `rosf`, `rosf-hw` and `rosf-oss-cad` stay on their own
  bases; rosf does not link z3, so the libz3 reason does not apply to it.
- **Base-image note:** the shared-base plan in
  [`2026-09-sva-image-one-suite.md`](2026-09-sva-image-one-suite.md) §Question 3 wrote
  `ARG BASE_IMAGE=rust:1.95-slim-bookworm` as the default for a parameterised `Dockerfile.dev`.
  If that PR lands, the default for **mununu** is now `-slim-trixie`; rosf's own images may keep
  bookworm — the "one suite tag everywhere" policy is about the oss-cad-suite release, not the
  Debian release.

### hw-verification-uba

- Nothing. `hw-verif` is not derived from `mununu-dev`.

### mununu-ui

- Nothing.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu/docker/Dockerfile.dev` (`mununu-dev`) | Base bookworm → trixie; libz3 4.8.12 → 4.13.3 | Yes if consumers pin a version tag — CI rebuilds it on every run (`cache-from: gha`) |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Inherits the new base; suite layer re-downloaded once (FROM changed) | Yes — mandatory (e2e re-run before merge; see *The measurement*) |
| `mununu/docker/Dockerfile` (production `mununu`) | Unchanged (still `rust:1.91-slim-bookworm`; see *Not covered here*) | No |
| `mununu/docker/Dockerfile.extract` | Unchanged | No |
| `mununu/docker/Dockerfile.extract-circt` | Placeholder, unchanged | No |
| `mununu/docker/Dockerfile.extract-llvm` | Placeholder, unchanged | No |
| `rosf/docker/Dockerfile.dev` (`rosf-dev`) | Unaffected (no z3) | No |
| `rosf/docker/Dockerfile` (`rosf`) | Unaffected | No |
| `rosf/docker/Dockerfile.hw` (`rosf-hw`) | Unaffected | No |
| `rosf/docker/Dockerfile.oss-cad` (`rosf-oss-cad`) | Unaffected | No |
| `monono/docker/Dockerfile.dev` (`monono-dev`) | Unaffected (no z3; suite tag unchanged) | No |
| `hw-verification-uba` (`hw-verif`) | Unaffected | No |
| `mununu-private/artifact/Dockerfile`, `Dockerfile.repro` | Unaffected by this PR (their own bases) | No |

## Shared footer — verify on your side

```bash
docker image inspect mununu-dev -f '{{ index .Config.Labels "org.opencontainers.image.base.name" }}' 2>/dev/null || true
docker run --rm mununu-dev sh -c 'cat /etc/debian_version; dpkg -s libz3-dev | grep ^Version'
# trixie: "13.x" / "Version: 4.13.3-1". bookworm would print "12.x" / "4.8.12-3.1".
```

## Provenance

- Base bump: pending merge — see branch `docker/trixie-base-libz3` (this briefing ships with it).
- Motivation: mununu#568 (`z3` 0.20 → 0.21, RUSTSEC-2026-0295), merged 2026-09-22 as `aa428b0`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- Prior briefing on this image: [`2026-09-sva-image-one-suite.md`](2026-09-sva-image-one-suite.md).

## Not covered here

- **The production Dockerfiles** (`docker/Dockerfile`, `docker/Dockerfile.extract`) still build on
  `rust:1.91-slim-bookworm` and install no `libz3-dev` in their builder stage, although the `z3`
  crate has been a mandatory, system-linked dependency since Phase A.4. Nobody builds them in CI.
  They most likely fail at link time today; fixing them (libz3-dev in the builder, `libz3-4` in the
  runtime stage, and the same trixie base for the same reason) is a separate PR, unverified here.
- The shared-base (`BASE_IMAGE`) parameterisation of `Dockerfile.dev` from the previous briefing.
- `z3-sys` detects the library version through pkg-config or a `z3` binary on `PATH`; the image has
  neither (libz3-dev ships no `.pc` file and the `z3` CLI is not installed), so detection falls
  back to the assumed minimum, 4.13.3 — which on trixie is also the truth. Installing the `z3`
  package to make detection explicit is a possible follow-up, not done here.
