#!/usr/bin/env bash
# validate.sh — R46-6 / GAP-2 regression: a WIDE field concretized to a
# small value set fits the cap (effective-bits accounting) AND its
# escape-to-OOB no longer reports a spurious VIOLATED (realize
# numericity-gate fix).
#
# Contract, in three checks:
#   1. WITHOUT the sidecar, `wide7.sv`'s 24-bit `cnt` is REJECTED
#      (StateSpaceOverflow: 24 > MAX_STATE_BITS = 20) — proving the raw
#      design does not fit and the concretization is load-bearing.
#   2. WITH the sidecar (`bounded_counter bound=7` → 3 effective bits),
#      the design lifts: the effective-bits cap accounting (GAP-2) admits
#      it where raw width did not.
#   3. The two reachability properties target in-set, genuinely-reachable
#      values (cnt == 1, cnt == 7) and come back SATISFIED + non-vacuous.
#      Before the realize OOB-marker fix the escape-to-OOB at 7→8 produced
#      an OOB sink whose `__mununu_oob__` valuation tripped the numericity
#      gate, disabling atom binding and reporting a spurious VIOLATED.
#
# Like the R46-5 fixture, the BTOR2 is produced in mununu's own temp dir
# (the yosys adapter), so nothing cap-busting lands under examples/ where
# the `btor2_kmts_lift_sweep` glob would trip on it. This script writes
# only its own report JSON, to a mktemp dir.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
OUT="$(mktemp -d -t mununu-r46-6-XXXXXX)"
trap 'rm -rf "${OUT}"' EXIT

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

export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

echo "=== R46-6 — wide field concretized + escape-to-OOB, no spurious VIOLATED ==="
echo "fixture: ${SRC}/wide7.sv (SYNTHETIC; 24-bit counter, bounded_counter bound=7)"
echo

cd "${SRC}"

# Check 1 — without the sidecar the raw 24-bit design must be REJECTED.
echo "--- (1) raw 24-bit (no sidecar) must hit the state-bit cap ---"
sed 's#sidecar = "wide7.mununu.json"##' verify.toml > "${OUT}/nosidecar.toml"
if "${MUNUNU}" verify --fail-on none --base-dir "${SRC}" "${OUT}/nosidecar.toml" > "${OUT}/nosidecar.out" 2>&1; then
  echo "FAIL: raw 24-bit design verified instead of hitting the cap" >&2
  exit 1
fi
if ! grep -qiE "state bits|max supported|StateSpaceOverflow|2\^20" "${OUT}/nosidecar.out"; then
  echo "FAIL: no-sidecar run did not report a state-bit cap overflow" >&2
  tail -5 "${OUT}/nosidecar.out" >&2
  exit 1
fi
echo "ok: raw 24-bit rejected (cap overflow) — concretization is load-bearing"
echo

# Check 2 + 3 — with the sidecar, the design fits and both properties are SAT.
echo "--- (2,3) concretized (bound=7) lifts; reachable targets SATISFIED ---"
"${MUNUNU}" verify --fail-on none --json verify.toml > "${OUT}/verify.json" 2>"${OUT}/verify.err" || {
  echo "FAIL: mununu verify exited non-zero with the sidecar" >&2
  tail -15 "${OUT}/verify.err" >&2
  exit 1
}

python3 - "${OUT}/verify.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
verds = {v["name"]: v for v in r.get("property_verdicts", [])}
assert verds, "FAIL: no property verdicts"
for name in ("reach_t1", "reach_t7"):
    v = verds.get(name)
    assert v, f"FAIL: missing verdict {name}"
    assert (v["outcome"] == "holds"), f"FAIL: {name} not SATISFIED (the spurious-VIOLATED regression)"
    assert v["total_states"] > 0, f"FAIL: vacuous verdict for {name}"
    assert v["satisfying_states"] > 0, f"FAIL: 0 satisfying states for {name}"
    print(f"verdict {name}: SAT {v['satisfying_states']}/{v['total_states']} over {v['over']}")
print("R46-6 done-criteria met: wide field concretized fits the cap, the "
      "escape-to-OOB no longer disables atom binding, both reachable "
      "targets SATISFIED + non-vacuous.")
PY

echo
echo "=== R46-6 VALIDATION PASSED ==="
