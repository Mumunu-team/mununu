# mununu-profile — the dev image plus valgrind, for callgrind profiles kcachegrind can open.
#
# Why a separate image: valgrind is a profiling tool, not a build or verification dependency, and
# the dev image is the thing CI rebuilds on every toolchain bump. Keeping it out of `mununu-dev`
# keeps that image's contract (and its size) unchanged.
#
# Build (the dev image must exist first — see docs/dev-container.md):
#   docker build -f docker/Dockerfile.profile -t mununu-profile .
#
# Use, through the driver (scripts/profile_cases.py profile --tool callgrind …), or by hand:
#   docker run --rm -v "$(pwd)":/work -v mununu-target:/ct -w /work -e CARGO_TARGET_DIR=/ct \
#     mununu-profile cargo build --profile profiling -p mununu-cli        # once, Linux binary → /ct/profiling/mununu
#   docker run --rm -v "$(pwd)":/work -v mununu-target:/ct -w /work mununu-profile \
#     valgrind --tool=callgrind --callgrind-out-file=/work/target/profiles/x.callgrind.out \
#     /ct/profiling/mununu --quiet btor2 verify-recoverability design.btor2 --target "s == 0"
#
# callgrind slows a run 20–50×, so profile a case calibrated to a few seconds native, or use
# --toggle-collect='*ExactModel*' to count only inside the fixpoint. The `.callgrind.out` file
# opens in kcachegrind (Linux) or qcachegrind (`brew install qcachegrind`, macOS); the driver
# also writes a `callgrind_annotate --inclusive=yes` text summary next to it.
ARG BASE_IMAGE=mununu-dev:latest
FROM ${BASE_IMAGE}

USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends valgrind \
 && rm -rf /var/lib/apt/lists/* \
 && valgrind --version

WORKDIR /work
