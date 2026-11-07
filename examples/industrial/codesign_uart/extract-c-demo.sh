#!/usr/bin/env bash
# extract-c-demo.sh — run `mununu codesign extract-c` against the
# Document C §C.4 firmware.c and emit the synthesised CTXDSL automaton.
#
# This script is intentionally separate from `validate.sh`. validate.sh
# produces a byte-deterministic transcript that has to reproduce on any
# machine — adding a clang shell-out would make the transcript depend on
# whether the user's host has clang installed. extract-c-demo.sh is the
# opt-in demonstration of the new code path; run it on a machine with
# clang on $PATH.
#
# Usage:
#   ./examples/industrial/codesign_uart/extract-c-demo.sh
#
# The script does three things:
#   1. Sanity-check that clang is on $PATH.
#   2. Run `mununu codesign extract-c firmware.c --register-map register_map.json
#      --synthesize-automaton` and print the JSON output.
#   3. Print, for each function the extractor walked, the synthesised
#      `automaton_ctxdsl` field side-by-side with the hand-authored
#      automaton in `firmware.ctxdsl` so a reader can compare them.
#
# What slice 2.b currently produces is a *linear* automaton: one state
# per register access in source order. The hand-authored CTXDSL has a
# richer shape (Polling self-loop, reset transitions, internal tick).
# That gap is slice 2.c's frontier; this script makes the gap visible
# so the reader can see exactly what automation gives you today vs.
# what hand-authoring adds.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

EXAMPLE_DIR="examples/industrial/codesign_uart"
FIRMWARE_C="$EXAMPLE_DIR/firmware.c"
REGISTER_MAP="$EXAMPLE_DIR/register_map.json"
HAND_AUTHORED="$EXAMPLE_DIR/firmware.ctxdsl"

if ! command -v clang >/dev/null 2>&1; then
    echo "clang not found on \$PATH" 1>&2
    echo "install via xcode-select --install (macOS) or 'apt install clang' (Linux)" 1>&2
    exit 1
fi

echo "build: mununu binary (cargo)" 1>&2
cargo build --quiet --bin mununu

printf '\n=== Step 1: extract-c against firmware.c with --synthesize-automaton ===\n'
printf '$ mununu codesign extract-c %s --register-map %s --synthesize-automaton\n' \
    "$FIRMWARE_C" "$REGISTER_MAP"
./target/debug/mununu codesign extract-c \
    "$FIRMWARE_C" \
    --register-map "$REGISTER_MAP" \
    --synthesize-automaton

printf '\n=== Step 2: hand-authored CTXDSL (firmware.ctxdsl) for comparison ===\n'
printf '$ cat %s\n' "$HAND_AUTHORED"
cat "$HAND_AUTHORED"

printf '\n=== Comparison notes ===\n'
cat <<'EOF'
The extractor now runs against LLVM IR (clang -emit-llvm -S), not
clang's AST. The IR-based backend:
  - Recognises polling loops at the IR level via back-edge analysis
    (a basic block with a BrCond terminator whose back-edge body is
    empty + a volatile load whose result feeds the condition). The
    matched read is marked `flow: polling_loop`, producing a Loop_i
    state with three transitions on the same access label.
  - Collapses bit-field read-modify-store sequences (clang lowers
    `CTRL.bit.tx_start = 1` to a load → and → or → store quartet)
    into a single write to the touched field, identified by the
    AND-mask complement.
  - Handles BOTH the `extern volatile T *const NAME` form AND the
    `#define NAME ((T *)0xADDR)` form (the canonical STM32/NXP/Microchip
    HAL convention). The IR has the address either way; the parser
    extracts it from `getelementptr (...inttoptr (i64 N to ptr)...)`
    constexprs as well as from SSA `getelementptr` instructions.

For `uart_send` the synthesised shape is S0 → Loop0 ⤴ → S1 → S2 → S3,
matching the hand-authored Init/Polling/Ready/Sending up to
state-name renaming. Two structural gaps remain — both deliberate,
neither blocking:
  - Internal `tick` self-loops modelling cycles spent polling between
    status reads. The C source has no syntactic anchor for these.
  - Explicit `reset` transitions for system-level recovery. Reset is
    an environment event, not a firmware-source construct.

Anything beyond the recognised idioms still falls back to linearisation
with a structured warning. The hand-authored CTXDSL remains the
canonical model whenever synthesis cannot represent a construct
faithfully — see docs/archive/design/c-extraction-correctness-scope.md.
EOF
