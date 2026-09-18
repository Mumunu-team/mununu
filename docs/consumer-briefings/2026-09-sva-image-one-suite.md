# Consumer briefing — `mununu-sva` now pins the same oss-cad-suite release as rosf and monono

> **Audience:** **monono** (its formal lane runs inside this image via `make formal-docker`;
> its `tools/versions.lock` carries a *reported* tier for it); **rosf** (asked in the same
> handoff to publish a shared base — the label convention and the layer-sharing constraint below
> are for that work); **hw-verification-uba** (its `hw-verif` image is now the one image on the
> host pinned to the older release); **mununu-ui** (no impact — read the table and stop).
>
> This answers monono's handoff of 2026-09-18 (`monono/docs/handoff-image-merge.md`, "Prompt for
> the `mununu` agent"). Each of its four questions gets a section.

## TL;DR

`mununu-sva` used to take its EDA suite from the sibling `hw-verif` image with
`COPY --from=hw-verif:latest` — an image built once, in June 2026, from the **2025-12-31**
oss-cad-suite (yosys 0.60+70, Verilator 5.043) and never published. It now downloads the
**2026-08-24** release straight from YosysHQ's public releases — **the tag `rosf-hw` and
`monono-dev` already pin** — asserts every version claim against the binaries at build time, and
exposes the pins as image labels.

**No Rust changed.** What changed is the compiler under the lift. The e2e sweep was run in the
old and the new image at the same commit; the result is in *The measurement* below.

**Action for monono:** rebuild the image (one command, below); `tools/check-versions.sh --images`
then reports full parity, and your reported tier can become enforced if you want it to. Your
recorded stamps (`metrics.md`, the cards that say "yosys 0.60+70") describe the old image; the
next run re-stamps them. **Action for rosf:** none for the image you have; read *A shared base*
before publishing one. **Action for hw-verification-uba:** consider bumping `Dockerfile`'s
`OSS_CAD_SUITE_TAG` to `2026-08-24` so Verilator replays run on the same Verilator as everything
else (not done here — not mununu's file).

## What changed, tool by tool

The tools `mununu` locates as subprocesses (`locate_yosys` / `locate_slang` / `locate_slang_plugin`
/ `locate_sv2v` / `locate_verilator` / `locate_btormc` / `locate_pono` / `locate_cvc5`), old image →
new image, measured on 2026-09-18 by running each binary in each image:

| tool | role in a verdict | `mununu-sva` before | `mununu-sva` now | `rosf-hw` / `monono-dev` |
|---|---|---|---|---|
| oss-cad-suite tag | — | 2025-12-31 (via `hw-verif`) | **2026-08-24** | 2026-08-24 |
| yosys | the SV → BTOR2 lift | 0.60+70 | **0.68+120** | 0.68+120 |
| yosys-slang plugin (`slang.so`) | `--frontend slang` lift (`read_slang`) | from the 2025-12-31 suite | **from the 2026-08-24 suite** | same file |
| slang CLI | SVA extraction (`--ast-json`) | 11.0.0 (separate GitHub release download) | **11.0.448+e222e7dc0 (the suite's)** | same file |
| Verilator | counterexample replay gate | 5.043 | **5.051** | 5.051 |
| sv2v | SV-2017 → V-2005 normaliser | v0.0.13 (GitHub release) | v0.0.13 — unchanged | not in the suite; not carried |
| btormc | reachability oracle / portfolio | 3.2.4 | 3.2.4 — unchanged | 3.2.4 |
| cvc5 | Craig interpolation (SyGuS) | 1.0.1-dev.2.77d0bec48 | unchanged | same |
| pono | portfolio member | (prints no version) | (prints no version) — **treat as changed** | — |
| z3 (binary; the Rust binary links the system libz3, untouched) | — | 4.15.5 | 4.15.5 | 4.15.5 |

Two decisions inside that table are mununu's, and are now written in the Dockerfile:

- **slang comes from the suite, not from a separate download.** One language front-end, and the
  `slang.so` plugin yosys loads for `read_slang` is built from the same snapshot, so the SVA
  extractor and the RTL lift cannot disagree about the language. The suite is a dated snapshot, so
  the tag pins slang as firmly as a release tag did.
- **The version ARGs are claims, and the build checks them.** `YOSYS_VERSION=0.68`,
  `VERILATOR_VERSION=5.051`, `SLANG_VERSION=11.0`, `SV2V_VERSION=v0.0.13` are grepped against
  `yosys -V` / `verilator --version` / `slang --version` / `sv2v --version` in a build step that
  fails on mismatch. A label that repeats an ARG is therefore a fact about the tree.

## The measurement

<!-- SWEEP-RESULTS -->

## Question 1 — was there a reason to hold yosys at 0.60?

**No.** There was no pin and no decision. `Dockerfile.sva` copied `/opt/oss-cad-suite` out of
whatever `hw-verif:latest` happened to be on the contributor's machine, and `hw-verif` had been
built once, on 2026-06-04, from the 2025-12-31 release. The 0.60 that monono's lock carries an
argument for was a stale sibling image. The Dockerfile header now records this so the next
person does not re-derive it, and monono's lock can drop the argument: the justification for
mununu's pin lives in mununu's Dockerfile, where it belongs.

The Dockerfile header also fixes the bump procedure: change the tag, read the versions the
assertion step prints, write them into the claims, run the sweep, ship a briefing. A lift-compiler
change is validated by the lift, not by the version string.

## Question 2 — labels

Read them without running anything:

```sh
docker image inspect mununu-sva \
  -f '{{ range $k, $v := .Config.Labels }}{{ $k }}={{ $v }}{{ "\n" }}{{ end }}' \
  | grep '^io.github.mumunu-team'
# io.github.mumunu-team.oss-cad-suite.tag=2026-08-24
# io.github.mumunu-team.oss-cad-suite.date=20260824
# io.github.mumunu-team.yosys.version=0.68
# io.github.mumunu-team.verilator.version=5.051
# io.github.mumunu-team.slang.version=11.0
# io.github.mumunu-team.sv2v.version=v0.0.13

docker image inspect mununu-sva -f '{{ index .Config.Labels "io.github.mumunu-team.yosys.version" }}'
```

Plus the standard `org.opencontainers.image.{title,description,source,base.name}`.

**Suggested convention for the other two images**, so monono's `check-versions.sh --images` can
assert all three the same way: the same key *suffixes* (`oss-cad-suite.tag`, `oss-cad-suite.date`,
`yosys.version`, `verilator.version`) under the publishing repo's own reverse-DNS namespace — or
adopt these keys verbatim; mununu has no objection to the prefix being shared. What matters is
that the value is an `ARG` the build asserted, not a string someone typed.

## Question 3 — rebase onto a shared base (`rosf-oss-cad:<tag>`)?

**Not as a `FROM`, and not yet — here is the constraint, the measurement behind it, and what would
make mununu adopt one.**

**Layer sharing needs a common ancestor; `COPY --from` does not share.** Docker stores a layer
under its *chain* ID, so two images share bytes and page cache only when one is `FROM` the other
(or both descend from the same image). `COPY --from=<image>` creates a *new* layer in the copying
image — that is exactly what the old `Dockerfile.sva` did with `hw-verif`, and `docker history`
shows the price: a 2.47 GB `COPY /opt/oss-cad-suite` layer in `mununu-sva` *in addition to* the
2.47 GB in `hw-verif`. "Reusing" the sibling never saved a byte.

**`mununu-sva`'s ancestor is `mununu-dev`, and `mununu-dev` cannot carry the suite.** The
subprocess-tools-are-not-bundled policy keeps EDA tools out of the build/test image (it is the CI
gate; the suite is 2.6 GB the Rust workspace never touches). So `mununu-sva` can only make a
suite base its ancestor by *inverting* its chain: `FROM <suite-base>` and then installing the
Rust toolchain, libz3, cargo-nextest, gdb, … on top — i.e. a second copy of `Dockerfile.dev`'s
recipe. That trades a 2.6 GB duplicated suite for a ~2 GB duplicated Rust toolchain and a forked
dev recipe, which is the class of drift this whole handoff is trying to remove.

**What mununu ships today instead** captures two of the three measured benefits: one tag pinned
identically in all three Dockerfiles (policy true everywhere, and now *checkable* via labels), and
no dependency on an unpublished image (which also un-broke the e2e nightly). The disk / page-cache
benefit is not captured — one extraction per image, ~2.6 GB each. For scale: today's
`docker system df` shows 36 GB reclaimable, almost all of it dangling images and build cache,
not the suite.

**What would make `mununu-sva` adopt a base — a separate, measured PR:** parameterise
`Dockerfile.dev` on its base image (`ARG BASE_IMAGE=rust:1.95-slim-bookworm` + a rustup bootstrap
that is a no-op on the official image), so `mununu-sva` can be *the same recipe* built on
`FROM <suite-base>`. Then the base becomes the ancestor of all three images with no forked recipe.
Expected saving on this host: ~2.6 GB per rebased image plus shared page cache across concurrent
sessions.

**Where the base should live, if built:** the base itself is trivially `debian:bookworm-slim` +
the tarball at `/opt/oss-cad-suite`, **off `PATH`** (monono's point about two Verilators is
right), labelled as above. rosf is a fine owner for a *local* tag. But note the one consumer that
cannot use a local tag: **mununu's CI e2e nightly**, which runs on a GitHub runner. It needs
either a registry-published base (`ghcr.io/…/oss-cad-suite:<tag>`; nobody publishes one today —
`ghcr.io/mumunu-team/mununu-sva` does not exist either, despite two docs mentioning it) or the
public tarball download this PR uses. So: if a registry base appears, `Dockerfile.sva` points at
it; until then the public download **is** the CI-safe base, and pinning the same tag is the
sharing that actually holds.

## Question 4 — the 109 GB `mununu-target` volume and per-worker target directories

Measured 2026-09-18 (`du` over the volume):

| path | size | what |
|---|---|---|
| `/cargo-target/debug` | **99 GB** | `--all-features` *test* builds: 52 GB `deps`, **47 GB `incremental`** |
| `/cargo-target/release` | 1.6 GB | the release build monono consumes |
| `/cargo-target/release/mununu` | 20 MB | the binary `formal-docker` actually runs |

Three facts change the picture monono's handoff paints:

1. **cargo's lock is per profile directory** (`debug/.cargo-lock`, `release/.cargo-lock`), and
   `make formal-docker` invokes no cargo at all — it runs `/cargo-target/release/mununu` directly.
   So a formal run never contends with an e2e sweep (debug profile). Only two concurrent
   **release builds** serialise. The "second session blocks with no output" case is real but
   narrower than "any two sessions building mununu".
2. **The 99 GB is cache, not state.** Nothing in it needs to survive; the 47 GB of `incremental`
   is pure cost in a volume that is only ever rebuilt after large diffs. Setting
   `CARGO_INCREMENTAL=0` for volume-backed builds would roughly halve the volume at no verdict
   cost; the e2e workflow now does that on the runner. Not changed for the local volume here —
   contributors iterating in a persistent container do benefit from incremental, and that trade
   is theirs.
3. **`mununu-target-w1` / `mununu-target-w1stats` / `mununu-bisect-target` / `mununu-cargo-home`
   are referenced by nothing** in mununu, mununu-private, monono or rosf (0 links each, ~0.4 GB
   each). They were a per-worker experiment that did not become a convention. Per-worker
   *volumes* are the wrong shape anyway: each is a full copy of the cache, and the only thing a
   consumer needs to share is a 20 MB binary.

**mununu's view, for monono to consume:** the durable fix is not a volume layout but **a baked
binary** — an image variant that builds `mununu` at a known commit into `/usr/local/bin` and needs
no volume, so `formal-docker` never depends on a cargo target directory at all. That changes
`formal-docker`'s contract (it currently checks `/cargo-target/release/mununu`) and reintroduces
the stale-binary trap monono's `doctor` already guards against, so it is proposed, not done. In
the meantime: one shared `mununu-target` for release builds is correct; give a *debug*/test
workload its own volume only if it must run concurrently with a release build, and prune
`/cargo-target/debug` freely when disk matters.

## Per-consumer

### monono

- **What to update:** rebuild the image —
  ```sh
  cd $MUNUNU_REPO && docker build -f docker/Dockerfile.dev -t mununu-dev . \
    && docker build -f docker/Dockerfile.sva -t mununu-sva .
  ```
  then `tools/check-versions.sh --images` reports `mununu-sva yosys 0.68 — full parity`. The
  `MUNUNU_YOSYS_EXPECTED` reported tier can become enforced if you want it; the measured argument
  for tolerating 0.60 in `versions.lock` can be retired (see Question 1). Re-run the gate; where a
  card or `metrics.md` says "yosys 0.60+70", the next run re-stamps it.
- **What to expect:** identical verdicts on the blocks measured (see *The measurement*). Your own
  lock already records byte-identical BTOR2 for `slot_arbiter` and `tmds_encoder` across 0.60 /
  0.68. Rule 5 of your lock's bump procedure applies: if a verdict *does* move, that is the
  finding.
- **Report parsing:** no shape change. Nothing in the JSON or the report text depends on the
  suite version.
- **The `--env` stamp:** `check-versions.sh --env` will now print slang `11.0.448…` from
  `/opt/oss-cad-suite/bin/slang` instead of `11.0.0` from `/usr/local/bin/slang`; there is no
  second slang on the image any more.

### rosf

- **What to update:** nothing in `rosf-hw`. If you publish the base the handoff asks for, use the
  label convention in Question 2 and keep the suite off `PATH`; the constraint in Question 3 says
  when mununu can `FROM` it.

### hw-verification-uba

- **What to update (recommended, not required):** `Dockerfile`'s `OSS_CAD_SUITE_TAG` /
  `OSS_CAD_SUITE_STAMP` to `2026-08-24` / `20260824`, so `target-executor` Phase 3.5 replays
  under the same Verilator as the e2e replay gate. Nothing in mununu depends on `hw-verif` any
  more.

### mununu-ui

- **What to update:** nothing. No wire-format change.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| mununu `Dockerfile` (prod CLI/server) | not involved; carries no EDA tools | No |
| mununu `Dockerfile.dev` (`mununu-dev`) | untouched; `mununu-sva` still extends it | No |
| mununu `Dockerfile.extract` | not involved | No |
| mununu `Dockerfile.extract-circt` / `.extract-llvm` | placeholders, not built | No |
| mununu `Dockerfile.sva` (`mununu-sva`) | **the change** — suite source, tool versions, labels | **Yes — mandatory**; the e2e sweep was re-run in it (below) |
| `mununu-sva-pono` (mununu-private, `FROM mununu-sva`) | inherits the new suite; its own gmp/mpfr/pono-msat layers unchanged | **Yes** on next use — it rebuilds on top of the new base |
| `hw-verif` (hw-verification-uba) | no longer consumed by mununu; still 2025-12-31 | No (optional bump recommended above) |
| rosf `docker/Dockerfile.hw` (`rosf-hw`) | already on 2026-08-24; untouched | No |
| rosf `docker/Dockerfile` / `Dockerfile.dev` | not involved | No |
| monono `docker/Dockerfile.dev` (`monono-dev`) | already on 2026-08-24; untouched | No |

## Test the transition

```sh
# 1. the image says what it carries
docker image inspect mununu-sva -f '{{ index .Config.Labels "io.github.mumunu-team.oss-cad-suite.tag" }}'
# 2026-08-24

# 2. the tools agree with the labels (the build already asserted this)
docker run --rm mununu-sva sh -c 'yosys -V; verilator --version; slang --version; sv2v --version'

# 3. one real SVA design, end to end, in the image (vendored fixtures; fully reproducible)
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target mununu-sva \
  cargo test -p mununu-core --lib --all-features e2e_csrng_real_sva_verdict_breakdown -- --ignored --nocapture

# 4. the whole slang-gated suite, one test per process
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target mununu-sva make e2e
```

## Provenance

- Handoff: `monono/docs/handoff-image-merge.md` (2026-09-18).
- Change: `docker/Dockerfile.sva`, `.github/workflows/e2e.yml` (nightly restored),
  `docker/README.md`, `CLAUDE.md` §"SVA-verification e2e validation", `docs/dev-container.md`,
  `docs/external-tools.md`. PR: pending merge — see branch `feat/sva-image-one-suite`.
- Related: #529 (nightly disabled, and the two restore options — this is option (a)), mununu#503
  (how the ignored set drifted), mununu#504 (one test per process).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md), trigger 6.

## Not covered here

- **The shared base image itself.** Not built; Question 3 states the constraint and the PR shape
  that would let `mununu-sva` adopt one.
- **A baked-binary `mununu-sva` variant** that frees `formal-docker` from the cargo volume.
  Proposed in Question 4, not built.
- **Pruning the stray `mununu-target-w1*` / `mununu-bisect-target` / `mununu-cargo-home`
  volumes or the 99 GB debug cache.** Measured, not deleted — that is the host owner's call.
- **`hw-verification-uba`'s pin.** Recommended, not changed; not mununu's file.
- **`ghcr.io/mumunu-team/mununu-sva`** is mentioned in `wiki/CI-and-Agent-Integration.md` and
  `docs/verifying-rtl.md` but is not published. Pre-existing; untouched here.
