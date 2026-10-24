#!/usr/bin/env bash
# validate.sh — H.5-GR1 assume-guarantee recoverability showcase on OpenTitan
# csrng_main_sm, decided by the exact-symbolic engine.
#
# Recoverability — "from every reachable state, can the FSM get back to MainSmIdle?" —
#     always_recoverable = nu Y. ((mu X. (idle || <> X)) && [] Y)   (AG EF idle)
# is a there-exists-a-path (EF) property that SVA structurally cannot phrase. The
# exact-symbolic engine decides it over the full bit-blasted state: 2-valued and
# definite, never ⊥. The verdict flips on a single explicit environment assumption:
#
#   * local_escalate_i FREE            -> VIOLATED (definite): a local security
#       escalation drives the FSM to the terminal MainSmError trap (csrng_main_sm.sv
#       lines 54-56; the trap is stable, per the design's own SVA
#       CsrngMainErrorStStable_A), from which idle is unreachable.
#   * assume  G !local_escalate_i      -> HOLDS (definite): with no escalation the FSM
#       always cycles back to idle. The assumption is applied as an input
#       concretization (--pin local_escalate_i=0).
#
# This is the mununu-exclusive wedge: an assume-guarantee liveness verdict on a
# branching-time property, on real silicon, that flips on the explicit assumption —
# and it is decided exactly, so both HOLDS and VIOLATED transfer to the modeled design
# (Bruns-Godefroid; here trivially, since the exact engine uses no abstraction).
#
# Pedigree: csrng_main_sm.sv is real OpenTitan RTL (Apache-2.0), vendored + pinned
# under ../m2_opentitan_csrng_main_sm/source/ (M.2); the prim_assert macros come from
# ../m0_opentitan_prim_arbiter/source/ (M.0). This is a design-pattern demonstration
# of the property class on real hardware, NOT a vulnerability finding — the
# escalation-latching behaviour is the SEC_CM design intent.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
CSRNG="examples/verify/m2_opentitan_csrng_main_sm/source"
PRIM="examples/verify/m0_opentitan_prim_arbiter/source"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in sv2v yosys; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "validate.sh: required tool '${tool}' not on PATH" >&2; exit 2; }
done

OUT="$(mktemp -d -t mununu-h5gr1-XXXXXX)"
trap 'rm -rf "${OUT}"' EXIT

# The recoverability property is a mununu-exclusive annotation (distinct from the
# design's own SVA) — prepend it as a @mununu_guarantee. idle = (state_q == 55 =
# MainSmIdle = 6'b110111; MainSmError = 6'b101001 = 41 — csrng_pkg.sv).
{ echo '// @mununu_guarantee nu Y. ((mu X. ((state_q == 55) or <> X)) and [] Y)'
  cat "${CSRNG}/csrng_main_sm.sv"; } > "${OUT}/csrng_main_sm.sv"

SRC=(--source "${CSRNG}/csrng_pkg.sv"
     --source "${PRIM}/prim_assert.sv"
     --source "${PRIM}/prim_assert_standard_macros.svh"
     --source "${PRIM}/prim_assert_sec_cm.svh"
     --source "${PRIM}/prim_flop_macros.sv")

run() {
  # --preprocess-sv2v: run sv2v first (as the reset-gated exact path expects), which
  # resolves the prim_assert include dispatch and flattens before yosys.
  "${MUNUNU}" sv verify-auto "${OUT}/csrng_main_sm.sv" "${SRC[@]}" \
    --top csrng_main_sm --engine exact-symbolic --preprocess-sv2v "$@"
}

echo "=== H.5-GR1 — csrng recoverability (AG EF MainSmIdle), exact-symbolic ==="
echo
echo "--- no assumption (local_escalate_i free) — expect VIOLATED ---"
run > "${OUT}/free.out" 2>&1 || true
grep -E "ann_guarantee" "${OUT}/free.out" || true
echo
echo "--- assume G !local_escalate_i (--pin local_escalate_i=0) — expect HOLDS ---"
run --pin local_escalate_i=0 > "${OUT}/assumed.out" 2>&1 || true
grep -E "ann_guarantee" "${OUT}/assumed.out" || true
echo

ok=1
grep -qE "ann_guarantee.*: VIOLATED" "${OUT}/free.out" \
  || { echo "FAIL: no-assumption run expected VIOLATED" >&2; ok=0; }
grep -qE "ann_guarantee.*: HOLDS" "${OUT}/assumed.out" \
  || { echo "FAIL: no-escalation-assumption run expected HOLDS" >&2; ok=0; }
[[ "${ok}" == "1" ]] || exit 1

echo "=== H.5-GR1 VALIDATION PASSED ==="
echo "csrng recovers to MainSmIdle under the no-escalation assumption and fails once a"
echo "security escalation is admitted — an assume-guarantee branching-time (AG EF)"
echo "verdict SVA cannot phrase, decided exactly (2-valued, no bottom) on real OpenTitan RTL."
