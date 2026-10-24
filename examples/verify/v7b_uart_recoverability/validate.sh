#!/usr/bin/env bash
# validate.sh — V.7-b recoverability showcase on OpenTitan uart_tx, with a
# COMPOUND idle predicate.
#
# Asks a branching-time question SVA cannot state: "while transmitting, can the
# UART always drain its frame and get back to idle?" — i.e. recoverability,
#     always_recoverable = nu Y. ((mu X. (idle || <> X)) && [] Y)   (AG EF idle)
# over a predicate abstraction of the real RTL. Unlike the csrng showcase
# (V.7-c), uart_tx's idle is NOT a single state-enum value — it is a COMPOUND
# condition over two registers:
#     idle = (bit_cnt_q == 0) && (sreg_q == 2047)   # counter drained AND shift reg back to its idle pattern
# declared in source/idle.mununu.json. The cube lift routes compounds through the
# eager SmtAllPairs may-relation + SMT hyper-must edges (B.1 + B.2), so the
# alternating-fixpoint (νμ) verdict is clean-sound — no soundness-tag.
#
# The contrast is bug-vs-fix, NOT reset-dependence (uart_tx self-recovers, so
# both reset variants of the correct design hold). We tie tx_enable=1 and
# rst_ni=1 (actively transmitting, no external reset) so the verdict reflects the
# design's OWN ability to drain a frame:
#
#   * upstream uart_tx              -> always_recoverable HOLDS (definite-TRUE):
#       the bit counter decrements each baud tick, so every started frame drains
#       back to idle.
#   * planted-bug uart_tx_stuck.sv -> always_recoverable VIOLATED (definite-FALSE):
#       the ONE-LINE change removes the decrement, so a started frame never drains
#       and the FSM cannot return to idle.
#
# Both verdicts are definite (no KleeneBot) and sound for the model
# (Bruns-Godefroid preservation; the GKMTS hyper-must edges make the νμ verdict
# monotone under refinement per Shoham-Grumberg LMCS 2007).
#
# Pedigree: uart_tx.sv is real OpenTitan RTL (Apache-2.0), vendored + pinned
# under ../m1_opentitan_uart_tx/source/ (M.1). This fixture reuses that source
# for the correct variant and ships a deliberately broken copy
# (source/uart_tx_stuck.sv) for the bug variant. The planted bug is a
# DESIGN-PATTERN DEMONSTRATION of the recoverability property class, NOT a
# vulnerability finding — OpenTitan's real UART is correct.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
GOOD="examples/verify/m1_opentitan_uart_tx/source/uart_tx.sv"
BUG="examples/verify/v7b_uart_recoverability/source/uart_tx_stuck.sv"
SIDECAR="examples/verify/v7b_uart_recoverability/source/idle.mununu.json"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in sv2v yosys; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "validate.sh: required tool '${tool}' not on PATH" >&2; exit 2; }
done

OUT="$(mktemp -d -t mununu-v7b-XXXXXX)"
FORMULA='nu Y. ((mu X. (idle || <> X)) && [] Y)'
# Bootstrap predicate (busy = mid-frame) + the compound idle from the sidecar +
# SMT hyper-must edges for a clean-sound νμ verdict.
ARGS=(--predicate 'busy:bit_cnt_q == 10' --sidecar "${SIDECAR}" --must-edge-inference smt-hyper-must)

# Lift one variant to BTOR2 with tx_enable + rst_ni tied (actively transmitting,
# no external reset) and run CEGAR.
run_variant() {
  local name="$1" src="$2"
  sv2v "${src}" > "${OUT}/${name}.v" 2>"${OUT}/${name}.sv2v.err"
  yosys -q -p "read_verilog -sv ${OUT}/${name}.v; hierarchy -check -top uart_tx; proc; \
               flatten; connect -set tx_enable 1'b1; connect -set rst_ni 1'b1; \
               async2sync; dffunmap; setundef -anyconst; write_btor ${OUT}/${name}.btor2" \
    2>"${OUT}/${name}.yosys.err"
  "${MUNUNU}" btor2 cegar "${OUT}/${name}.btor2" --formula "${FORMULA}" "${ARGS[@]}" \
    > "${OUT}/${name}.out" 2>&1 || true
  grep -iE "verdict cells|outcome" "${OUT}/${name}.out" || true
}

cell() { grep "verdict cells" "${OUT}/$1.out" | sed -E "s/.*$2=([0-9]+).*/\1/"; }

echo "=== V.7-b — recoverability of OpenTitan uart_tx (AG EF idle, compound idle) ==="
echo
echo "--- upstream uart_tx (correct) — expect HOLDS ---"
run_variant good "${GOOD}"
echo
echo "--- planted-bug uart_tx_stuck (counter never re-arms) — expect VIOLATED ---"
run_variant bug "${BUG}"
echo

GOOD_T="$(cell good T)"; GOOD_F="$(cell good F)"
BUG_T="$(cell bug T)"; BUG_F="$(cell bug F)"

ok=1
if [[ "${GOOD_F}" != "0" || "${GOOD_T:-0}" -lt 1 ]]; then
  echo "FAIL: upstream uart_tx expected HOLDS (T>=1, F=0), got T=${GOOD_T} F=${GOOD_F}" >&2; ok=0
fi
if [[ "${BUG_F:-0}" -lt 1 ]]; then
  echo "FAIL: planted-bug uart_tx_stuck expected VIOLATED (F>=1), got T=${BUG_T} F=${BUG_F}" >&2; ok=0
fi
# Clean-sound guard: neither verdict may carry the B.3.b νμ soundness-tag (the
# hyper-must edges from B.2 must have fired).
if grep -qiE "alternation depth .* no MustHyperOnly" "${OUT}/good.out" "${OUT}/bug.out"; then
  echo "FAIL: a verdict carried the B.3.b soundness-tag — hyper-must edges did not fire" >&2; ok=0
fi
[[ "${ok}" == "1" ]] || { rm -rf "${OUT}"; exit 1; }

rm -rf "${OUT}"
echo "=== V.7-b VALIDATION PASSED ==="
echo "The correct uart_tx can always drain a frame back to idle; the one-line"
echo "planted bug (counter never re-arms) makes idle unreachable mid-frame —"
echo "mununu states and soundly decides this branching (AG EF) recoverability"
echo "property with a COMPOUND idle predicate on real OpenTitan RTL, definite"
echo "both ways and free of any soundness-tag."
