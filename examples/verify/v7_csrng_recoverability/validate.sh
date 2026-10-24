#!/usr/bin/env bash
# validate.sh — V.7-c recoverability showcase on OpenTitan csrng_main_sm.
#
# Asks a branching-time question SVA cannot state: "from every reachable state,
# can the FSM still get back to MainSmIdle?" — i.e. recoverability,
#     always_recoverable = nu Y. ((mu X. (idle || <> X)) && [] Y)   (AG EF idle)
# over a predicate abstraction of the real RTL, with idle = (state_q == MainSmIdle)
# and err = (state_q == MainSmError), via the predicate-cube CEGAR path with
# SMT-proved (hyper-)must edges.
#
# The verdict depends entirely on whether reset is available, and that is the
# honest, interesting result — NOT a bug. csrng's recovery story rests on reset,
# exactly as its SEC_CM sparse-FSM-plus-alert design intends:
#
#   * reset available (rst_ni free)      -> always_recoverable HOLDS (definite-TRUE):
#       asserting reset returns the FSM to MainSmIdle from any state.
#   * reset held inactive (rst_ni tied)  -> always_recoverable VIOLATED (definite-FALSE):
#       the MainSmError state and the unreachable sparse encodings become
#       permanent traps once reset is taken away.
#
# Both verdicts are definite (no KleeneBot) and sound for the model (Bruns-Godefroid
# preservation over the audited Control::All / bare-modality / unbounded fragment).
#
# Pedigree: csrng_main_sm.sv is real OpenTitan RTL (Apache-2.0), vendored + pinned
# under ../m2_opentitan_csrng_main_sm/source/ (M.2). This fixture reuses that
# source rather than re-vendoring it. The recoverability property is a
# design-pattern demonstration of the property class on real silicon, not a
# vulnerability finding — the reset-dependence it surfaces is intended behaviour.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
SRC="examples/verify/m2_opentitan_csrng_main_sm/source"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in sv2v yosys; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "validate.sh: required tool '${tool}' not on PATH" >&2; exit 2; }
done

OUT="$(mktemp -d -t mununu-v7c-XXXXXX)"
FORMULA='nu Y. ((mu X. (idle || <> X)) && [] Y)'
PREDS=(--predicate 'idle:state_q == 55' --predicate 'err:state_q == 41' --must-edge-inference smt-hyper-must)

# MainSmIdle = 6'b110111 = 55 ; MainSmError = 6'b101001 = 41 (csrng_pkg.sv).
sv2v -I"${SRC}" "${SRC}/csrng_pkg.sv" "${SRC}/csrng_main_sm.sv" > "${OUT}/csrng.v" 2>"${OUT}/sv2v.err"

# Lift one variant to BTOR2 (reset free, or reset tied inactive) and run CEGAR.
run_variant() {
  local variant="$1" tie=""
  [[ "${variant}" == "held" ]] && tie="connect -set rst_ni 1'b1;"
  yosys -q -p "read_verilog -sv ${OUT}/csrng.v; hierarchy -check -top csrng_main_sm; proc; \
               flatten; ${tie} async2sync; dffunmap; setundef -anyconst; write_btor ${OUT}/${variant}.btor2" \
    2>"${OUT}/${variant}.yosys.err"
  "${MUNUNU}" btor2 cegar "${OUT}/${variant}.btor2" --formula "${FORMULA}" "${PREDS[@]}" \
    > "${OUT}/${variant}.out" 2>&1 || true
  grep -iE "verdict cells|outcome" "${OUT}/${variant}.out" || true
}

cell() { grep "verdict cells" "${OUT}/$1.out" | sed -E "s/.*$2=([0-9]+).*/\1/"; }

echo "=== V.7-c — recoverability of OpenTitan csrng_main_sm (AG EF MainSmIdle) ==="
echo
echo "--- reset available (rst_ni free) — expect HOLDS ---"
run_variant free
echo
echo "--- reset held inactive (rst_ni tied 1'b1) — expect VIOLATED ---"
run_variant held
echo

FREE_T="$(cell free T)"; FREE_F="$(cell free F)"
HELD_T="$(cell held T)"; HELD_F="$(cell held F)"

ok=1
if [[ "${FREE_F}" != "0" || "${FREE_T:-0}" -lt 1 ]]; then
  echo "FAIL: reset-available expected HOLDS (T>=1, F=0), got T=${FREE_T} F=${FREE_F}" >&2; ok=0
fi
if [[ "${HELD_T}" != "0" || "${HELD_F:-0}" -lt 1 ]]; then
  echo "FAIL: reset-held expected VIOLATED (T=0, F>=1), got T=${HELD_T} F=${HELD_F}" >&2; ok=0
fi
[[ "${ok}" == "1" ]] || { rm -rf "${OUT}"; exit 1; }

rm -rf "${OUT}"
echo "=== V.7-c VALIDATION PASSED ==="
echo "Recovery to MainSmIdle holds while reset is available and fails once reset is"
echo "withheld — mununu states and soundly decides this branching (AG EF) property"
echo "on real OpenTitan RTL, with definite verdicts both ways."
