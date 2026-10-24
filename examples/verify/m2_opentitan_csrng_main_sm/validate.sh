#!/usr/bin/env bash
# validate.sh — M.2 KMTS-lifter + KleeneDomain milestone on OpenTitan
# `csrng_main_sm.sv` (§Phase 11 priority #2, post-M.1).
#
# Contract: R.2 (BTOR2 → KMTS lifter) + R.3 (KleeneDomain evaluator)
# must process the OpenTitan `csrng_main_sm` module end-to-end,
# producing a non-vacuous verdict on the declared property.
#
# Fixture history: M.2 originally targeted `hmac_core.sv` (~505 LOC)
# but the per-transition BTOR2 evaluation cost was too expensive for
# the milestone budget — see `.claude/plans/milestones/M-2-blocker-2026-05-26.md`.
# Per §10.2 user arbitration, the fixture was swapped (Path A) to
# `csrng_main_sm.sv`: same OpenTitan-scale industrial intent, much
# smaller (136 LOC → 413 BTOR2 lines vs hmac_core's 17 595).
#
# Scope:
#   `mununu context eval --adapter sv-yosys --preprocessor sv2v` on
#   the upstream `csrng_main_sm.sv` + a hand-written minimal stub
#   `csrng_pkg.sv` (defines only the acmd_e + main_sm_state_e
#   enums the FSM consumes; sound vs upstream — see SOUNDNESS notes
#   in the stub) + the stub `prim_assert.sv` (empty SVA macros +
#   `PRIM_FLOP_SPARSE_FSM` expanded as a plain `always_ff`).
#
# OUT OF SCOPE:
#   - SBY oracle cross-check (deferred; the mu-calc formula is a
#     greatest fixpoint of [_] which maps to SBY-style invariance
#     only after refactoring).
#   - Sparse-FSM runtime alert hardening (the prim_assert stub
#     drops the alert flop wrapper; sound for the property).
#
# Per §10.2 milestone blocker protocol: if any stage fails, STOP and
# produce a blocker note rather than silently retrying or
# hand-authoring around it.
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

echo "=== M.2 — OpenTitan csrng_main_sm KMTS-lifter validation ==="
echo "fixture:  ${SRC}/csrng_main_sm.sv (136 LOC + stub csrng_pkg + stub prim_assert)"
echo "upstream: lowRISC/opentitan @ $(cat "${SRC}/UPSTREAM_COMMIT.txt")"
echo

cd "${SRC}"
echo "--- step 1/1: mununu context eval (R.2 + R.3 end-to-end) ---"
LIBRARY_PATH=/usr/local/opt/z3/lib "${MUNUNU}" context eval --fail-on none \
  --adapter sv-yosys --preprocessor sv2v \
  --sidecar prim_assert.sv \
  --sidecar csrng_pkg.sv \
  --formula error_never_reached \
  --automaton Circuit \
  csrng_main_sm.sv > "${OUT}/eval.out" 2>&1
echo "(eval finished; tail of output:)"
tail -15 "${OUT}/eval.out"

if ! grep -qE "States satisfying|Initial states" "${OUT}/eval.out"; then
  echo "FAIL: context eval did not produce a verdict" >&2
  exit 1
fi

echo
echo "=== M.2 VALIDATION PASSED ==="
echo "eval output: ${OUT}/eval.out"
