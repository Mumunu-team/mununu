# Dockerfile for mununu (core verification binary)
# Produces a lightweight image with the CLI + API server.
#
# Build:
#   docker build -f docker/Dockerfile -t mununu .
#
# Usage (CLI):
#   docker run --rm -v $(pwd):/work mununu context eval \
#     /work/spec.espec.json --formula safety --automaton Main
#
# Usage (server):
#   docker run -p 8080:8080 mununu server --addr 0.0.0.0:8080

# The builder matches docker/Dockerfile.bundle's: the pinned toolchain (rust-toolchain.toml) on
# Debian trixie, whose libz3 is 4.13.3 (z3-sys 0.13's floor). Before mununu#991 this stage was
# rust:1.91 on bookworm with no libz3 at all, and the link failed (`unable to find library -lz3`);
# it also copied the removed legacy `src/` (#925), which failed before compiling.
ARG RUST_VERSION=1.95
FROM rust:${RUST_VERSION}-slim-trixie AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        libz3-dev pkg-config build-essential \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
# Nothing in an image build reuses incremental state: this stage starts from a
# cold target dir and runs `cargo build` once, so incremental would only write an
# `incremental/` tree into the layer for a rebuild that never comes. (cargo's
# release profile is already non-incremental by default; this makes the intent
# explicit and holds if the profile or the build line ever changes.)
ENV CARGO_INCREMENTAL=0
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
# mununu#715 — `.git` is not in the build context (`.dockerignore`), so the commit is passed in:
#   docker build --build-arg MUNUNU_BUILD_SHA=$(git rev-parse HEAD) \
#     --build-arg MUNUNU_BUILD_DIRTY=$(test -z "$(git status --porcelain --untracked-files=no)" && echo 0 || echo 1) …
# Left empty, the binary reports no build identity (never a guess).
ARG MUNUNU_BUILD_SHA=""
ARG MUNUNU_BUILD_DIRTY=""
ENV MUNUNU_BUILD_SHA=${MUNUNU_BUILD_SHA} MUNUNU_BUILD_DIRTY=${MUNUNU_BUILD_DIRTY}
RUN cargo build --release -p mununu-cli

FROM debian:trixie-slim
# libz3-4: the binary links it. curl: the healthcheck. tini: PID 1 (mununu#991) — a timed-out
# subprocess member's forked solvers are re-parented to PID 1, and mununu does not reap them, so
# without an init each one stays a zombie for the server's lifetime
# (`scripts/check-container-reaps.sh` measures it).
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl libz3-4 tini \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -m appuser
COPY --from=builder /build/target/release/mununu /usr/local/bin/
USER appuser
HEALTHCHECK --interval=30s --timeout=5s CMD curl -sf http://localhost:8080/api/v1/health || exit 1
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "mununu"]
CMD ["server", "--addr", "0.0.0.0:8080"]
