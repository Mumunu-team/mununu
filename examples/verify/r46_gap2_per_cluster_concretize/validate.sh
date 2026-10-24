#!/usr/bin/env bash
# validate.sh — R46-6 / GAP-2: per-cluster slicing × param-concretization
# COMPOSE. This is the synthetic proof the GAP-2 de-risk called for, and
# the regression guard for the composed path.
#
# Contract, in two checks:
#   1. WITHOUT the sidecar the design is REJECTED (72 raw state bits) —
#      proving per-cluster slicing ALONE cannot rescue it: each cluster's
#      cone is still a 24-bit counter past MAX_STATE_BITS = 20.
#   2. WITH the sidecar (`bounded_counter bound=127` per counter), automatic
#      per-cluster verification fires (joint effective = 3 × 7 = 21 > 20),
#      slices each counter into its own cluster, and the concretization
#      shrinks that counter to 7 effective bits so the cluster fits. All
#      three reachability properties come back SATISFIED + non-vacuous,
#      each routed to a distinct cluster automaton (`Circuit__clK`).
#
# Like the R46-5 fixture, the BTOR2 is produced in mununu's own temp dir,
# so nothing cap-busting lands under examples/ where the
# btor2_kmts_lift_sweep glob would trip on it.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
OUT="$(mktemp -d -t mununu-r46-6pc-XXXXXX)"
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

echo "=== R46-6 — per-cluster slicing × param-concretization compose ==="
echo "fixture: ${SRC}/three_wide.sv (SYNTHETIC; three independent 24-bit counters)"
echo

cd "${SRC}"

# Check 1 — slicing alone (no sidecar) must not rescue the raw design.
echo "--- (1) no sidecar: raw 72-bit design must be rejected at the cap ---"
sed 's#sidecar = "three_wide.mununu.json"##' verify.toml > "${OUT}/nosidecar.toml"
if "${MUNUNU}" verify --base-dir "${SRC}" "${OUT}/nosidecar.toml" > "${OUT}/nosidecar.out" 2>&1; then
  echo "FAIL: raw design verified instead of hitting the cap" >&2
  exit 1
fi
if ! grep -qiE "state bits|max supported|StateSpaceOverflow|2\^20" "${OUT}/nosidecar.out"; then
  echo "FAIL: no-sidecar run did not report a state-bit cap overflow" >&2
  tail -5 "${OUT}/nosidecar.out" >&2
  exit 1
fi
echo "ok: raw design rejected — per-cluster slicing alone cannot rescue it"
echo

# Check 2 — slicing + concretization compose: per-cluster fires, all SAT.
echo "--- (2) sidecar: per-cluster × concretization compose; all SATISFIED ---"
"${MUNUNU}" verify --json verify.toml > "${OUT}/verify.json" 2>"${OUT}/verify.err" || {
  echo "FAIL: mununu verify exited non-zero with the sidecar" >&2
  tail -15 "${OUT}/verify.err" >&2
  exit 1
}

python3 - "${OUT}/verify.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))

# per-cluster fired: cluster_routing present on some source.
routing = None
for s in r.get("sources", []):
    routing = (s.get("partition_summary") or {}).get("cluster_routing")
    if routing:
        break
assert routing, "FAIL: no cluster_routing — per-cluster verification did not fire"
clusters = sorted(set(routing.values()))
assert len(clusters) >= 3, f"FAIL: expected >=3 cluster automata, got {clusters}"
print(f"per-cluster: routed {len(routing)} properties to {len(clusters)} clusters {clusters}")

verds = {v["name"]: v for v in r.get("property_verdicts", [])}
assert verds, "FAIL: no property verdicts"
overs = set()
for name in ("reach_a7", "reach_b7", "reach_c7"):
    v = verds.get(name)
    assert v, f"FAIL: missing verdict {name}"
    assert (v["outcome"] == "holds"), f"FAIL: {name} not SATISFIED (the spurious-VIOLATED regression)"
    assert v["total_states"] > 0, f"FAIL: vacuous verdict for {name}"
    assert v["over"].startswith("Circuit__cl"), \
        f"FAIL: {name} not routed to a cluster automaton (over={v['over']})"
    overs.add(v["over"])
    print(f"verdict {name}: SAT {v['satisfying_states']}/{v['total_states']} over {v['over']}")
assert len(overs) >= 3, f"FAIL: properties did not route to distinct clusters: {overs}"
print("R46-6 done-criteria met: per-cluster slicing × param-concretization "
      "compose — joint busts the cap, each wide cone is sliced out and "
      "concretized to fit, all three targets SATISFIED over distinct clusters.")
PY

echo
echo "=== R46-6 (per-cluster × concretization) VALIDATION PASSED ==="
