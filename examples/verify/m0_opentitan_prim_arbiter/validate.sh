#!/usr/bin/env bash
# validate.sh — M.0 pipeline-reach milestone on OpenTitan
# prim_arbiter_fixed.sv.
#
# Per the KMTS-pivot plan
# (.claude/plans/you-are-a-formal-vast-lake.md §10.3 M.0), this is
# the first industrially-realistic validation milestone. The contract:
# the R.0a + R.0b + R.0c stack must process a small, real OpenTitan
# RTL module without error.
#
# Scope today (pipeline reach only):
#   1. mununu sv preprocess: sv2v elaborates SV-2017 → Verilog-2005
#      across the prim_arbiter_fixed.sv + 6 prim_assert.sv/* include
#      chain. Output is non-empty.
#   2. mununu sv emit-btor2-per-module: Yosys-no-flatten emits one
#      BTOR2 per submodule. At least one BTOR2 lands on disk.
#
# RETIRED (M.5, 2026-06-20): the original step 3 ran
# `mununu sv compare-pipelines` (R.0c), which diffed the NATIVE-parser
# arm against the KMTS arm. The native SV parser was fully excised in
# S.2b (singular-pipeline commitment), so `compare-pipelines` — along
# with `sv_pipeline_compare::compare_pipelines` and
# `tests/sv_compare_pipelines.rs` — no longer exists: there is only one
# pipeline left to compare against itself. M.0's load-bearing capability
# is the frontend reach (steps 1+2), which the M.5 regression sweep
# confirms is intact.
#
# OUT OF SCOPE (deferred to later milestones):
#   - Property-verification verdict (M.1+; needs R.2 KMTS lifter +
#     R.3 KleeneDomain evaluator).
#   - SBY oracle comparison (M.1+; same dependency).
#   - Wrapper SV with `// @mununu` property annotation. M.0 verifies
#     the *frontend reach*, not the verifier.
#
# Per the milestone blocker protocol (§10.2): if any stage fails,
# STOP and produce .claude/plans/milestones/M-0-blocker-<date>.md
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

echo "=== M.0 — OpenTitan prim_arbiter_fixed pipeline-reach validation ==="
echo "fixture:  ${SRC}/prim_arbiter_fixed.sv (instantiated via prim_arbiter_fixed_m0_wrapper.sv at N=2, DW=2)"
echo "upstream: lowRISC/opentitan @ $(cat "${SRC}/UPSTREAM_COMMIT.txt")"
echo "note:     M.0 Fix B per M-0-blocker-2026-05-21.md — small-param wrapper around the upstream SV (unchanged)."
echo "          Scale to production N=8, DW=32 lands at M.1+ once R.2 KMTS lifter is on."

echo
echo "--- step 1/2: mununu sv preprocess (R.0a) ---"
"${MUNUNU}" sv preprocess \
  "${SRC}/prim_arbiter_fixed.sv" \
  "${SRC}/prim_arbiter_fixed_m0_wrapper.sv" \
  -I "${SRC}" \
  --output "${OUT}/elaborated.v" 2>&1 | tail -3
if [[ ! -s "${OUT}/elaborated.v" ]]; then
  echo "FAIL: sv preprocess produced empty output" >&2
  exit 1
fi
WC_ELAB="$(wc -l < "${OUT}/elaborated.v" | tr -d ' ')"
echo "PASS: sv preprocess emitted ${WC_ELAB} lines of Verilog-2005"

echo
echo "--- step 2/2: mununu sv emit-btor2-per-module (R.0b) ---"
# Top is the wrapper; the upstream module is included via --source so
# Yosys can resolve the instantiation. The hierarchy snapshot will
# enumerate both modules as submodules of the wrapper.
"${MUNUNU}" sv emit-btor2-per-module "${SRC}/prim_arbiter_fixed_m0_wrapper.sv" \
  --source "${SRC}/prim_arbiter_fixed.sv" \
  --top prim_arbiter_fixed_m0_wrapper \
  --frontend sv2v \
  --output-dir "${OUT}/btor2" 2>&1 | tail -8
N_BTOR="$(find "${OUT}/btor2" -name '*.btor2' 2>/dev/null | wc -l | tr -d ' ')"
if [[ "${N_BTOR}" -lt 1 ]]; then
  echo "FAIL: emit-btor2-per-module did not produce any BTOR2 files" >&2
  exit 1
fi
echo "PASS: emit-btor2-per-module produced ${N_BTOR} BTOR2 file(s)"

echo
echo "=== M.0 PIPELINE-REACH VALIDATION PASSED ==="
echo "elaborated.v:    ${OUT}/elaborated.v (${WC_ELAB} lines)"
echo "BTOR2 per-mod:   ${OUT}/btor2/*.btor2 (${N_BTOR} file(s))"
