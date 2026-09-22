# Reproducible Dev Container

> Concept: pinned Docker image and named cargo cache volume so every contributor and the CI runner execute identical commands.

`docker/Dockerfile.dev` pins the Rust toolchain and native build deps so every contributor and the CI runner execute the exact same commands. The toolchain version in the Dockerfile mirrors `rust-toolchain.toml`; see [`toolchain.md`](toolchain.md) for the bump procedure.

## Build the image

Rare — only on Dockerfile or toolchain changes.

```bash
docker build -f docker/Dockerfile.dev -t mununu-dev .
```

## Cargo cache volume

Create a named volume once so subsequent runs stay warm. The container writes to `/cargo-target` (set by `CARGO_TARGET_DIR` in the image), **not** to the host's `target/`. Host and container caches stay independent.

```bash
docker volume create mununu-target
```

If you skip this volume, the container compiles from scratch every run (no cache survives `--rm`). The host's `target/` is never written to by the container, so host-side `cargo` and container-side `make` do not contend for the same artifacts.

## Incremental compilation

> Source of truth: [`docker/Dockerfile.dev`](../docker/Dockerfile.dev) (`ENV CARGO_INCREMENTAL=0`) — surface: CLI-only — a build-environment setting, not a mununu verb

The image sets `CARGO_INCREMENTAL=0`. CI runs `make ci` with no volume at `/cargo-target`, so every run starts cold and incremental state would be written once and never read. With the `mununu-target` volume warm, incremental *does* speed up a re-run after a small edit to `mununu-core`; opt back in for that run:

```bash
docker run --rm -e CARGO_INCREMENTAL=1 -v $(pwd):/work -v mununu-target:/cargo-target mununu-dev make ci
```

## Ephemeral run

One-off command, warm cache via the named volume:

```bash
docker run --rm \
  -v $(pwd):/work \
  -v mununu-target:/cargo-target \
  mununu-dev make <verb>
```

The Makefile verbs are `build`, `test`, `lint`, `verify`, `ci` (= lint + test), and `clean`. Run `make help` for the index.

## Persistent container

Faster for iterative work:

```bash
docker run -d --name mununu-dev-c \
  -v $(pwd):/work \
  -v mununu-target:/cargo-target \
  mununu-dev sleep infinity

docker exec mununu-dev-c make <verb>

docker stop mununu-dev-c && docker rm mununu-dev-c
```

## CI-exact reproduction

```bash
docker build -f docker/Dockerfile.dev -t mununu-dev .
docker volume create mununu-target
docker run --rm -v $(pwd):/work -v mununu-target:/cargo-target mununu-dev make ci
```

If `make ci` passes inside this container, it passes in GitHub Actions.

## RTL counterexample validation (sibling image)

RTL trace reproduction uses the **`hw-verif:latest`** image from the sibling `../hw-verification-uba` repo (the OSS CAD Suite is too heavy for the mununu dev image). Per-target reproductions live under `.claude/reviews/prospector/staging/<TARGET>/repro/Makefile` and are invoked as:

```bash
docker run --rm -v $(pwd):/work hw-verif:latest make -C <leaf> sim
```

See the `target-executor` agent's Phase 3.5 and `.claude/reviews/prospector/staging/RTL-002/repro/` for the canonical pattern.

Since 2026-09-18 the opt-in `mununu-sva` image (`docker/Dockerfile.sva`) carries the same oss-cad-suite — tag-pinned to the release rosf and monono use, Verilator included — so the e2e suite's counterexample-replay gate runs there and the image no longer depends on `hw-verif` being built first. `hw-verif` stays the image the `target-executor` procedure names. `docker/README.md` shows how to read the toolchain pins off the `mununu-sva` image labels.
