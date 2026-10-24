#!/usr/bin/env bash
# validate_m6_ui_e2e.sh — M.6 milestone: UI end-to-end demo (§Phase 11 slot 7).
#
# M.6 is a UX-validation milestone: the slot-6 `/cegar` RefinementTracePanel
# (mununu-ui) loads a fixture, renders the 3-valued { T, F, ⊥ } verdict + the
# per-iteration refinement trace, and round-trips a predicate edit. The panel
# is a thin surface peer of `POST /api/v1/btor2/cegar` — it sends the BTOR2 +
# formula + predicates and renders the response verbatim. So this script
# automates the AUTOMATABLE half: it drives the EXACT HTTP endpoint the panel
# calls with the real M.4 Caliptra `soc_ifc_boot_fsm` fixture (pre_fix +
# post_fix BTOR2) and asserts the API reproduces the M.4 pre/post-distinguishing
# CWE-1245 verdict over the wire. The remaining half — visually confirming the
# panel renders that response — is the contributor's manual sign-off step (see
# the RUNBOOK echoed at the end).
#
# Relationship to the other M.4 scripts in this dir:
#   - validate.sh            : Phase-A.3 structural bit-blast check.
#   - validate_m4_cegar.sh   : M.4 — the CLI `mununu btor2 cegar` verdict.
#   - validate_m6_ui_e2e.sh  : M.6 — the SAME verdict through the HTTP API the
#                              UI panel consumes (this file).
#
# Per §10.2: if any stage fails, STOP and produce a blocker note rather than
# silently retrying / hand-authoring around it.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
ADDR="${MUNUNU_M6_ADDR:-127.0.0.1:8088}"
OUT="$(mktemp -d -t mununu-m6-XXXXXX)"
SERVER_PID=""
cleanup() {
  [[ -n "${SERVER_PID}" ]] && kill "${SERVER_PID}" 2>/dev/null || true
  rm -rf "${OUT}"
}
trap cleanup EXIT

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate_m6_ui_e2e.sh: mununu binary not found at ${MUNUNU}" >&2
  echo "                       build it first with: cargo build -p mununu-cli" >&2
  exit 2
fi
for tool in yosys sv2v curl python3; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "validate_m6_ui_e2e.sh: required tool '${tool}' not on PATH" >&2
    exit 2
  fi
done

echo "=== M.6 — UI end-to-end: M.4 Caliptra CEGAR through POST /api/v1/btor2/cegar ==="
echo "endpoint: http://${ADDR}/api/v1/btor2/cegar   (the RefinementTracePanel's backend)"
echo

# --- generate pre_fix + post_fix BTOR2 (identical to validate_m4_cegar.sh) ---
gen_btor2() {
  local variant="$1"
  sv2v -I"${SRC}" \
    "${SRC}/soc_ifc_pkg.sv" "${SRC}/soc_ifc_reg_pkg.sv" \
    "${SRC}/caliptra_2ff_sync.sv" "${SRC}/soc_ifc_boot_fsm_${variant}.sv" \
    > "${OUT}/${variant}.v" 2>"${OUT}/${variant}.sv2v.err"
  yosys -q -p "read_verilog -sv ${OUT}/${variant}.v; hierarchy -check -top soc_ifc_boot_fsm; \
               proc; flatten; async2sync; dffunmap; setundef -anyconst; write_btor ${OUT}/${variant}.btor2" \
    2>"${OUT}/${variant}.yosys.err"
}
echo "--- generating BTOR2 (sv2v + Yosys, anyconst power-up) ---"
gen_btor2 pre_fix
gen_btor2 post_fix
echo "pre_fix:  $(wc -l < "${OUT}/pre_fix.btor2" | tr -d ' ') BTOR2 lines"
echo "post_fix: $(wc -l < "${OUT}/post_fix.btor2" | tr -d ' ') BTOR2 lines"

# --- start the HTTP API server (the UI's backend) ---
echo
echo "--- starting mununu server on ${ADDR} ---"
LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}" "${MUNUNU}" server --addr "${ADDR}" \
  > "${OUT}/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 30); do
  curl -s -o /dev/null "http://${ADDR}/" 2>/dev/null && break
  sleep 1
done
if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
  echo "FAIL: server did not stay up" >&2
  tail -10 "${OUT}/server.log" >&2
  exit 1
fi
echo "server up (pid ${SERVER_PID})"

# --- POST each variant to the panel's endpoint; extract the T-cell count ---
post_variant() {
  local variant="$1"
  python3 - "${OUT}/${variant}.btor2" > "${OUT}/${variant}.req.json" <<'PY'
import json, sys
print(json.dumps({
  "content": open(sys.argv[1]).read(),
  "formula": "<> (p5 || p6 || p7)",
  "predicates": [
    {"name": "p5", "register": "boot_fsm_ns", "value": 5},
    {"name": "p6", "register": "boot_fsm_ns", "value": 6},
    {"name": "p7", "register": "boot_fsm_ns", "value": 7},
  ],
  "must_edge_inference": "smt-per-target",
}))
PY
  curl -s -X POST "http://${ADDR}/api/v1/btor2/cegar" \
    -H 'Content-Type: application/json' --data "@${OUT}/${variant}.req.json" \
    > "${OUT}/${variant}.resp.json"
  python3 - "${OUT}/${variant}.resp.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
v = r["cells"]
print(f'{v["true_cells"]} {v["unknown_cells"]}')  # "T ⊥" to stdout for the caller
import sys as _s; _s.stderr.write(
  f'  verdict T={v["true_cells"]} F={v["false_cells"]} ⊥={v["unknown_cells"]} '
  f'| terminated={r["terminated_with"]} | iterations={len(r["iterations"])}\n')
PY
}

echo
echo "--- POST pre_fix (bug-bearing) ---"
read -r PRE_T PRE_BOT < <(post_variant pre_fix)
echo "--- POST post_fix (fixed) ---"
read -r POST_T POST_BOT < <(post_variant post_fix)

echo
echo "pre_fix : T=${PRE_T} ⊥=${PRE_BOT}"
echo "post_fix: T=${POST_T} ⊥=${POST_BOT}"

if [[ -z "${PRE_T}" || -z "${PRE_BOT}" || -z "${POST_T}" || -z "${POST_BOT}" ]]; then
  echo "FAIL: could not parse a verdict from one of the API responses" >&2
  exit 1
fi
# SOUND done-criterion — mirrors validate_m4_cegar.sh (IR-track P3.4, 2026-06-22),
# but established over the HTTP endpoint the RefinementTracePanel calls.
# The original M.6 criterion (post_fix True==0, "hazard UNREACHABLE") was UNSOUND:
# it relied on the Skolem-collapsed `<>`→`[]` diamond. Under the corrected
# EXISTENTIAL `Control::All` diamond the {p5,p6,p7} cube cannot PROVE the fixed
# FSM safe — post_fix is genuinely KleeneBot. The milestone is the sound pre/post
# DISTINCTION (definite hazard → indefinite), now matched bit-for-bit with the
# CLI verdict (validate_m4_cegar.sh) over the wire.
if [[ "${PRE_T}" -ge 1 && "${PRE_BOT}" -eq 0 && "${POST_BOT}" -ge 1 ]]; then
  echo
  echo "=== M.6 API-E2E PASSED (sound pre/post distinction) ==="
  echo "The /api/v1/btor2/cegar endpoint (the RefinementTracePanel's backend)"
  echo "reproduces the M.4 pre/post-distinguishing CWE-1245 verdict over HTTP:"
  echo "  pre_fix : undefined-encoding latch DEFINITELY present (T=${PRE_T}, ⊥=0) — CWE-1245 detected (sound)."
  echo "  post_fix: undefined-encoding latch NO LONGER DEFINITE (⊥=${POST_BOT}) — the fix removes the definite hazard."
  echo "  NOTE: post_fix is KleeneBot, not definite-safe — the {p5,p6,p7} cube cannot soundly PROVE the fixed FSM safe."
else
  echo "FAIL: expected pre(T>=1, ⊥==0) and post(⊥>=1); got pre(T=${PRE_T},⊥=${PRE_BOT}) post(T=${POST_T},⊥=${POST_BOT})" >&2
  exit 1
fi

cat <<RUNBOOK

--- M.6 manual UI sign-off (the remaining half) ---
The API half above is automated. To complete M.6, view the panel render:

  1) terminal A:  LIBRARY_PATH=/usr/local/opt/z3/lib mununu server --addr ${ADDR}
  2) terminal B:  cd mununu-ui && VITE_API_URL=http://${ADDR} npm run dev
  3) browser:     open the dev URL, navigate to  /cegar
  4) paste the pre_fix BTOR2 (or upload it), set
       formula:    <> (p5 || p6 || p7)
       predicates: p5, boot_fsm_ns, 5
                   p6, boot_fsm_ns, 6
                   p7, boot_fsm_ns, 7
       must-edge inference: smt-per-target (∀∀)
     and click "Run CEGAR refinement".
  5) confirm the panel renders verdict  T 7 / F 1 / ⊥ 0  + the trace table;
     edit a predicate / re-run and confirm the verdict updates.

Component + render coverage is pinned by
  mununu-ui/src/components/v6/__tests__/RefinementTracePanel.test.tsx.
RUNBOOK
