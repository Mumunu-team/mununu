#!/usr/bin/env bash
# validate.sh — R46-5 / R.4.6 per-cluster verification regression.
#
# Contract: on a SYNTHETIC design whose JOINT cone busts the bit-blast
# state-bit cap (two independent 11-bit counters = 22 state bits > 20),
# `mununu verify` automatically falls back to PER-CLUSTER verification —
# it partitions the two properties by their disjoint cones, slices each
# cluster to its own 11-bit cone, and verifies it. Both reachability
# properties come back SATISFIED, routed to distinct cluster automata
# (`Circuit__cl0` / `Circuit__cl1`).
#
# This is a mechanism regression test (NOT an M-milestone): the design is
# hand-written, clearly labeled synthetic. It guards the end-to-end
# "joint busts cap, clusters fit" path (R46-2/R46-3 + the cone-SLICE
# soundness fix) against regression.
#
# Note: the BTOR2 is produced inside mununu's own temp dir (the yosys
# adapter), so nothing under examples/ is written — no cap-busting
# `.btor2` lands where the `btor2_kmts_lift_sweep` glob would trip on it.
# This script writes only its own report JSON, to a mktemp dir.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
OUT="$(mktemp -d -t mununu-r46-5-XXXXXX)"
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

echo "=== R46-5 — automatic per-cluster verification on a cap-busting design ==="
echo "fixture: ${SRC}/two_cones.sv (SYNTHETIC; two independent 11-bit counters = 22 state bits)"
echo

cd "${SRC}"
echo "--- mununu verify (joint busts cap → automatic per-cluster) ---"
LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}" "${MUNUNU}" verify --fail-on none --json verify.toml \
  > "${OUT}/verify.json" 2>"${OUT}/verify.err" || {
  echo "FAIL: mununu verify exited non-zero" >&2
  tail -15 "${OUT}/verify.err" >&2
  exit 1
}

# Human-readable lines (for the log).
LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}" "${MUNUNU}" verify --fail-on none verify.toml 2>/dev/null \
  | grep -iE "clustered-COI|per-cluster verification|SATISFIED|VIOLATED" || true
echo

python3 - "${OUT}/verify.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))

# Done-criterion 1: per-cluster verification fired (cluster_routing present
# on some source — only set when the joint design busted the cap).
routing = None
for s in r.get("sources", []):
    routing = (s.get("partition_summary") or {}).get("cluster_routing")
    if routing:
        break
assert routing, "FAIL: no cluster_routing — per-cluster verification did not fire"
clusters = sorted(set(routing.values()))
assert len(clusters) >= 2, f"FAIL: expected >=2 cluster automata, got {clusters}"
print(f"per-cluster: routed {len(routing)} properties to {len(clusters)} clusters {clusters}")

# Done-criterion 2: both properties SATISFIED, non-vacuous, each routed to
# a distinct cluster automaton.
verds = {v["name"]: v for v in r.get("property_verdicts", [])}
assert verds, "FAIL: no property verdicts"
overs = set()
for name, v in verds.items():
    assert (v["outcome"] == "holds"), f"FAIL: {name} not SATISFIED"
    assert v["total_states"] > 0, f"FAIL: vacuous verdict for {name}"
    assert v["over"].startswith("Circuit__cl"), \
        f"FAIL: {name} not routed to a cluster automaton (over={v['over']})"
    overs.add(v["over"])
    print(f"verdict {name}: SAT {v['satisfying_states']}/{v['total_states']} over {v['over']}")
assert len(overs) >= 2, f"FAIL: properties did not route to distinct clusters: {overs}"
print("R46-5 done-criteria met: joint cone busts the cap, per-cluster "
      "verification rescues it, both verdicts SAT + non-vacuous over distinct clusters.")
PY

echo
echo "=== R46-5 VALIDATION PASSED ==="
