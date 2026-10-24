#!/usr/bin/env bash
# validate.sh — V.1 4-router NoC liveness (progress vs contention). §Phase 7
# domain validation; controlling doc docs/design/industrial-value-and-validation-domains.md §4.
#
# For each scheduling discipline:
#   1. Re-generate into a tmp dir and diff against the checked-in progress/
#      contention/ (the generator is deterministic).
#   2. Run `mununu verify` and assert the verdicts:
#        deadlock_freedom      = true  (both)
#        liveness_possible     = true  (both — AG EF delivered)
#        hop_bound             = true  (both — hops never exceed the diameter)
#        delivered_at_diameter = true  (both — delivered at exactly 2 hops)
#        liveness_inevitable   = true  under progress / false under contention
#      The last is the discriminating verdict: strong inevitability (AF) holds
#      only when the scheduler is fair; an unfair stall path starves the flit.
#
# The model is a generated design-pattern demonstration of the NoC-liveness
# domain, NOT a finding about a real interconnect (claims-integrity). The
# single-flit abstract `wait` over-approximates contention from other flows;
# the liveness verdicts are reported under exactly that reading.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v1_noc_mesh_4router"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: cargo build -p mununu-cli" >&2
  exit 2
fi

# assert_verdicts <model> <expected liveness_inevitable: true|false>
assert_verdicts() {
  local model="$1" expect_inev="$2"
  local jf
  jf="$(mktemp)"
  "${MUNUNU}" verify "${DIR}/${model}/verify.toml" --json 2>/dev/null >"${jf}"
  python3 -c '
import json, sys
model, expect_inev = sys.argv[2], sys.argv[3]
r = json.load(open(sys.argv[1]))
got = {v["name"]: (v["outcome"] == "holds") for v in r["property_verdicts"]}
want = {
    "deadlock_freedom": True,
    "liveness_possible": True,
    "hop_bound": True,
    "delivered_at_diameter": True,
    "liveness_inevitable": (expect_inev == "true"),
}
ok = True
for name, exp in want.items():
    g = got.get(name)
    print("    {:<22} {}  (expected {})".format(name + ":", g, exp))
    if g is not exp:
        ok = False
        sys.stderr.write("FAIL ({}): {} expected {}, got {}\n".format(model, name, exp, g))
if not ok:
    sys.exit(1)
' "${jf}" "${model}" "${expect_inev}"
  local rc=$?
  rm -f "${jf}"
  return $rc
}

echo "=== V.1 4-router NoC liveness — progress vs contention ==="
for entry in "progress true" "contention false"; do
  set -- $entry
  model="$1"; expect_inev="$2"
  echo "--- ${model} (expect liveness_inevitable=${expect_inev}) ---"
  TMP="$(mktemp -d)"
  python3 "${DIR}/generate.py" "${model}" "${TMP}" >/dev/null
  if ! diff -rq "${DIR}/${model}" "${TMP}" >/dev/null 2>&1; then
    echo "FAIL: checked-in ${model}/ differs from generate.py output — re-run the generator and commit." >&2
    diff -rq "${DIR}/${model}" "${TMP}" || true
    rm -rf "${TMP}"; exit 1
  fi
  rm -rf "${TMP}"
  assert_verdicts "${model}" "${expect_inev}"
done

echo
echo "=== V.1 VALIDATION PASSED ==="
echo "Single flit through a 2x2 mesh: deadlock-free, delivery always reachable, and"
echo "bounded to the diameter (2 hops) over the hop counter — in both disciplines."
echo "Strong inevitability (every path delivers) holds only under the fair scheduler:"
echo "the unfair stall path starves the flit, so mununu reports liveness_inevitable=false."
