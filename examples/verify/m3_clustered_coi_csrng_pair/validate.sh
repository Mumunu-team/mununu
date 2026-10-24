#!/usr/bin/env bash
# validate.sh — M.3 clustered-cone-of-influence milestone on real
# OpenTitan RTL (§Phase 11 slot 7, R4W-4 / R4W-5).
#
# Contract: the verify `sv-yosys` route runs the automated pipeline
# (sv2v → Yosys-no-flatten-then-flatten → BTOR2 → bit-blast → 3-valued
# eval) on two independent instances of the real OpenTitan
# `csrng_main_sm` sparse FSM, and the report carries a clustered
# cone-of-influence comparison whose max per-cluster cone is strictly
# smaller than the naive joint COI (the M.3 reduction), with non-vacuous
# property verdicts.
#
# Fixture shape (M-milestone pattern): the verified behaviour
# (`csrng_main_sm`) is real upstream RTL, vendored + pinned by
# UPSTREAM_COMMIT.txt; the harness `csrng_main_sm_pair.sv` and the stub
# packages `csrng_pkg.sv` / `prim_assert.sv` are hand-written, clearly
# labeled NOT vendored.
#
# OUT OF SCOPE: SBY oracle cross-check (deferred per the M.0-M.2
# precedent; done-criterion is a non-vacuous sound verdict + a measured
# cone reduction, not an SBY cross-check).
#
# Per §10.2 milestone blocker protocol: if any stage fails, STOP and
# produce a blocker note rather than silently retrying / hand-authoring.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
OUT="${THIS_DIR}/build"

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

rm -rf "${OUT}" && mkdir -p "${OUT}"

echo "=== M.3 — clustered COI on two OpenTitan csrng_main_sm instances ==="
echo "fixture:  ${SRC}/csrng_main_sm_pair.sv (harness) + csrng_main_sm.sv (vendored)"
echo "upstream: lowRISC/opentitan @ $(cat "${SRC}/UPSTREAM_COMMIT.txt")"
echo

cd "${SRC}"
echo "--- mununu verify (sv2v + flatten + bit-blast + clustered COI) ---"
LIBRARY_PATH=/usr/local/opt/z3/lib "${MUNUNU}" verify --fail-on none --json verify.toml > "${OUT}/verify.json" 2>"${OUT}/verify.err" || {
  echo "FAIL: mununu verify exited non-zero" >&2
  tail -15 "${OUT}/verify.err" >&2
  exit 1
}

echo "(verify finished; clustered-COI comparison + verdicts:)"
LIBRARY_PATH=/usr/local/opt/z3/lib "${MUNUNU}" verify --fail-on none verify.toml 2>/dev/null | grep -iE "clustered-COI|SATISFIED|VIOLATED" || true
echo

# Done-criterion 1: a clustered-COI comparison is present with a real
# reduction (max cluster cone < joint cone).
python3 - "${OUT}/verify.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
cc = None
for s in r.get("sources", []):
    cc = (s.get("partition_summary") or {}).get("cluster_coi")
    if cc:
        break
assert cc, "FAIL: no cluster_coi in the verify report"
joint = cc["joint_cone_size"]
mx = cc["max_cluster_cone_size"]
nclusters = len(cc["clusters"])
print(f"clustered-COI: joint cone {joint}, {nclusters} cluster(s), max cluster cone {mx}")
assert nclusters >= 2, f"FAIL: expected >=2 clusters, got {nclusters}"
assert mx < joint, f"FAIL: no reduction (max {mx} !< joint {joint})"
# Done-criterion 2: non-vacuous verdicts (the model is real; every
# property evaluated over a non-empty state space).
verds = r.get("property_verdicts", [])
assert verds, "FAIL: no property verdicts"
for v in verds:
    assert v["total_states"] > 0, f"FAIL: vacuous verdict for {v['name']}"
    print(f"verdict {v['name']}: {'SAT' if v['satisfied'] else 'VIOL'} "
          f"{v['satisfying_states']}/{v['total_states']}")
print("M.3 done-criteria met: clustered cones reduce the binding cone "
      f"({joint} -> {mx}) on real RTL, verdicts non-vacuous.")
PY

echo
echo "=== M.3 VALIDATION PASSED ==="
echo "report: ${OUT}/verify.json"
