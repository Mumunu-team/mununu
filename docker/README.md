# `docker/` — image catalogue

| File | Purpose | Status |
|---|---|---|
| `Dockerfile.dev` | **Reproducible dev/test image.** Rust workspace toolchain only — source is mounted, not COPYed. Same `make <verb>` command works locally and in CI. | Active |
| `Dockerfile.sva` | **SVA-verification validation image** (`mununu-sva`). `mununu-dev` + ONE tag-pinned [oss-cad-suite](https://github.com/YosysHQ/oss-cad-suite-build) release (yosys + the yosys-slang plugin, slang, Verilator, btormc, pono, cvc5) + sv2v. Runs the `#[ignore]`d slang-gated e2e suite (`make e2e`) and is the image downstream consumers (monono `make formal-docker`) verify in. Opt-in; not the CI gate. | Active |
| `Dockerfile` | Production image. Multi-stage build, copies source, ships the `mununu` CLI/server binary as the entrypoint. | Active |
| `Dockerfile.extract` | Production image for the `mununu-extract` tree-sitter frontend. Same multi-stage pattern. | Active |
| `Dockerfile.extract-circt` | Placeholder for a future CIRCT-based SystemVerilog extraction frontend. | Not yet implemented |
| `Dockerfile.extract-llvm` | Placeholder for a future LLVM/SVF-based C/C++/Rust extraction frontend. | Not yet implemented |

## Quick reference

```sh
# dev/test (the canonical local + CI workflow)
docker build -f docker/Dockerfile.dev -t mununu-dev .
docker volume create mununu-target   # one-time, warm cargo cache across runs
docker run --rm \
  -v $(pwd):/work \
  -v mununu-target:/cargo-target \
  mununu-dev make ci
# incremental compilation is off in the image (CI runs cold); for a tight local
# edit-test loop on the warm volume, opt back in for that run: -e CARGO_INCREMENTAL=1

# SVA e2e (needs mununu-dev first; ~2.6 GB suite download on first build)
docker build -f docker/Dockerfile.sva -t mununu-sva .
docker run --rm -v $(pwd):/work -v mununu-target:/cargo-target mununu-sva make e2e

# production CLI/server
docker build -f docker/Dockerfile -t mununu .
docker run -p 8080:8080 mununu server --addr 0.0.0.0:8080

# production extract
docker build -f docker/Dockerfile.extract -t mununu-extract .
docker run --rm -v $(pwd):/work mununu-extract \
  /work/config.extract.json --source /work/server.ts --output /work/spec.espec.json
```

## What `mununu-sva` carries — ask the image, not the tools

The toolchain pins are build `ARG`s in `Dockerfile.sva`, **asserted against the
binaries at build time** (a wrong claim fails the build), and repeated verbatim as
image labels. A consumer can therefore assert what it is running in without a
`docker run`:

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

# one key, for a script:
docker image inspect mununu-sva -f '{{ index .Config.Labels "io.github.mumunu-team.yosys.version" }}'
```

The suite tag is pinned to the SAME release as the sibling images that lift or
synthesise the same RTL (rosf `docker/Dockerfile.hw`, monono `docker/Dockerfile.dev`),
so one compiler produces the netlist that is verified, synth-checked and programmed.
Bump procedure and history are in the Dockerfile header.

## Sibling image: `hw-verif`

RTL counterexample-trace validation in the `target-executor` agent's Phase 3.5 uses
the sibling `hw-verif:latest` image from `../hw-verification-uba`. `mununu-sva` now
carries the same suite (Verilator included), so the e2e suite's replay gate runs
there; `hw-verif` is kept out of `Dockerfile.dev` for the same reason the suite is —
the Rust workspace itself does not need a 2.6 GB EDA layer.
