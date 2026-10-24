#!/usr/bin/env bash
# The productized no-sidecar verb: `mununu sv verify-auto` extracts the design's
# own SVA and checks it. Here it proves the arbiter's mutual-exclusion guarantee
# on the assembled system. Requires a `mununu` binary + slang/sv2v/yosys — this
# runs it from the mununu-sva image's built binary (build once:
#   docker run --rm -v "$PWD/../..":/work -v mununu-target:/cargo-target \
#     -e CARGO_TARGET_DIR=/cargo-target mununu-sva cargo build -p mununu-cli).
set -euo pipefail
cd "$(dirname "$0")"
cat master.sv system_checked.sv gr1_controller.sv > _checked.sv
docker run --rm -v mununu-target:/cargo-target -v "$PWD":/demo mununu-sva \
  /cargo-target/debug/mununu sv verify-auto /demo/_checked.sv --top system_checked --frontend sv2v \
  2>&1 | grep -E 'verify-auto:|assert\]|HOLDS|VIOLATED|state register'
rm -f _checked.sv
