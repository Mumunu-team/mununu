#!/usr/bin/env bash
# validate.sh — V.6 R.6.7 proof-of-fire validation on the hand-authored
# AMBA-style arbiter (Option B fixture per the 2026-06-09 R.6.7
# fixture-path analysis; see README.md in this directory).
#
# Per the V.6 done-criteria (R.6 plan §1 sub-item 6.7), this script
# demonstrates that mununu's R.2.5 predicate-cube lifter + R.6.6
# controllability-aware label dispatch + R.6.3 modality-aware modal
# step all run end-to-end against the AMBA-arbiter BTOR2 fixture
# via the CLI's R.6.6 `--controllable` flag (shipped same
# session as this script).
#
# Scope today:
#   1. mununu btor2 cegar end-to-end run with --controllable-input
#      flags + the {burst==0} predicate set. Terminates with
#      `Converged` (the loop finds no `KleeneBot` residual after the
#      first iteration on the trivial νX. <true> X liveness probe).
#   2. mununu btor2 cegar with a different predicate set + JSON
#      output for downstream consumers.
#
# OUT OF SCOPE (next R.6.7 session per master roadmap §11.4):
#   - Mu-calculus property authoring beyond the example probes here
#     (mutual exclusion safety + GR(1) liveness require state-side
#     predicates on `grant_0` / `grant_1`; these aren't in the
#     `burst`-only predicate set today; authoring them is the next
#     V.6 session).
#   - mununu-ui integration via `/api/v1/context/import` extension.
#     The CLI path is shipped today; the UI path is queued.
#   - SBY oracle cross-check (the modality-aware verdict has no
#     direct SBY equivalent; oracle comparison is a research follow-up).
#
# Per CLAUDE.md §10.2 milestone blocker protocol: if any stage fails,
# STOP and produce a blocker note rather than silently retrying or
# hand-authoring around it.

set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
BTOR2="${THIS_DIR}/source/amba_arbiter.btor2"
OUT="${THIS_DIR}/build"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu binary not found at ${MUNUNU}" >&2
  echo "             build it first with: cargo build -p mununu-cli" >&2
  exit 2
fi

if [[ ! -f "${BTOR2}" ]]; then
  echo "validate.sh: BTOR2 fixture missing at ${BTOR2}" >&2
  exit 2
fi

rm -rf "${OUT}" && mkdir -p "${OUT}"

echo "=== V.6 (R.6.7) — AMBA arbiter controllability-aware KMTS validation ==="
echo "fixture:  ${BTOR2}"
echo

# --- Step 1: CEGAR refinement on the trivial liveness probe with the ---
# --- controllability-aware lift (R.6.6 --controllable flag).   ---
echo "--- step 1/2: mununu btor2 cegar w/ R.6.6 controllability-aware lift ---"
LIBRARY_PATH=/usr/local/opt/z3/lib "${MUNUNU}" btor2 cegar \
  "${BTOR2}" \
  --formula 'nu X. < true > X' \
  --predicate 'burst_zero:burst=0' \
  --controllable ctrl_g0 \
  --controllable ctrl_g1 \
  --max-iterations 4 > "${OUT}/cegar_liveness.out" 2>&1

echo "(stdout tail:)"
tail -10 "${OUT}/cegar_liveness.out"
echo

if ! grep -qE "CEGAR refinement loop completed" "${OUT}/cegar_liveness.out"; then
  echo "FAIL: CEGAR loop did not complete cleanly" >&2
  echo "      full output: ${OUT}/cegar_liveness.out" >&2
  exit 1
fi

# --- Step 2: same fixture, JSON output for programmatic consumers. ---
echo "--- step 2/2: mununu btor2 cegar w/ JSON output ---"
LIBRARY_PATH=/usr/local/opt/z3/lib "${MUNUNU}" btor2 cegar \
  "${BTOR2}" \
  --formula 'nu X. [ true ] X' \
  --predicate 'burst_zero:burst=0' \
  --controllable ctrl_g0 \
  --controllable ctrl_g1 \
  --max-iterations 4 \
  --json > "${OUT}/cegar_safety.json" 2>&1

if ! grep -qE '"iterations"' "${OUT}/cegar_safety.json"; then
  echo "FAIL: JSON output missing expected 'iterations' field" >&2
  echo "      full output: ${OUT}/cegar_safety.json" >&2
  exit 1
fi

echo "(JSON head:)"
head -c 400 "${OUT}/cegar_safety.json" | python3 -m json.tool 2>/dev/null \
  | head -20 || head -c 400 "${OUT}/cegar_safety.json"
echo
echo

echo "=== V.6 VALIDATION PASSED ==="
echo "outputs:"
echo "  ${OUT}/cegar_liveness.out  — CEGAR loop log for νX. <true> X"
echo "  ${OUT}/cegar_safety.json   — JSON CEGAR trace for νX. [true] X"
echo
echo "Next R.6.7 session items (per master roadmap §11.4):"
echo "  - Mu-calculus property authoring for mutual exclusion + GR(1) liveness"
echo "    on richer predicate sets (state-side predicates on grant_0 / grant_1)."
echo "  - mununu-ui SV-source workflow + tutorial (web UI integration)."
echo "  - Documentation downgrade in industrial-value-and-validation-domains.md §8.5"
echo "    + proof-by-fire-findings.md row 5 (SYNTCOMP → hand-authored)."
