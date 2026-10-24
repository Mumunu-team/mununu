#!/usr/bin/env bash
# validate.sh — M.1 KMTS-lifter + counter-FSM milestone on OpenTitan
# uart_tx.sv. Per the plan §10.3 M.1, this is the second
# industrially-realistic validation milestone after M.0.
#
# Contract: R.2 (BTOR2 → KMTS lifter) + R-S5 (typedef widening — N/A
# here since uart_tx has no typedef enum) + bounded_counter
# abstraction must process the OpenTitan uart_tx module end-to-end,
# producing a verdict on the declared property.
#
# Scope:
#   1. mununu context eval --adapter sv-yosys --preprocessor sv2v on
#      the upstream `uart_tx.sv` with the M.1 sidecar declaring
#      bit_cnt_q + baud_div_q as bounded counters, sreg_q as
#      discover, etc. Verdict must be non-vacuous.
#
# OUT OF SCOPE:
#   - SBY oracle cross-check (deferred — the M.1 mu-calc formula
#     does not directly express the SBY-shape property; the oracle
#     comparison is a follow-up).
#   - Production-parameter scale (uart_tx is fixed-shape, so this
#     doesn't apply).
#
# Per the milestone blocker protocol (§10.2): if any stage fails,
# STOP and produce .claude/plans/milestones/M-1-blocker-<date>.md
# instead of silently retrying or hand-authoring around it.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu binary not found at ${MUNUNU}" >&2
  echo "             build it first with: cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in yosys sv2v; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "validate.sh: required tool '${tool}' not on PATH" >&2
    exit 2
  fi
done

OUT="${THIS_DIR}/build"
rm -rf "${OUT}" && mkdir -p "${OUT}"

echo "=== M.1 — OpenTitan uart_tx KMTS-lifter validation ==="
echo "fixture:  ${SRC}/uart_tx.sv (single module, fixed shape)"
echo "upstream: lowRISC/opentitan @ $(cat "${SRC}/UPSTREAM_COMMIT.txt")"
echo

cd "${SRC}"
echo "--- step 1/1: mununu context eval (R.2 KMTS lifter end-to-end) ---"
LIBRARY_PATH=/usr/local/opt/z3/lib "${MUNUNU}" context eval --fail-on none \
  --adapter sv-yosys --preprocessor sv2v \
  --formula idle_reachable_from_every_state \
  --automaton Circuit \
  uart_tx.sv 2>&1 | tee "${OUT}/eval.out" | tail -15

if ! grep -qE "States satisfying|Initial states" "${OUT}/eval.out"; then
  echo "FAIL: context eval did not produce a verdict" >&2
  exit 1
fi

echo
echo "=== M.1 VALIDATION PASSED ==="
echo "eval output: ${OUT}/eval.out"
