#!/usr/bin/env bash
# validate.sh — R46-6 / GAP-2 at K=5 on REAL OpenTitan RTL: per-cluster
# slicing × param-concretization across five independent detectors.
#
# Fixture: five `sysrst_ctrl_detect` instances (vendored OpenTitan, pinned
# in source/UPSTREAM_COMMIT.txt) wrapped by a hand-written harness. Each
# detector is one of sysrst_ctrl's debounce/detect sub-blocks and carries
# the vendored block's 32-bit detect/debounce timer counter (`cnt_q`) —
# the exact wide field the R.4.6 plan named as the sysrst cap-buster.
#
# Contract, in three checks (two load-bearing negatives + composed):
#   1. NO sidecar → REJECTED at 172 raw state bits (5 × 32-bit timers +
#      FSM flops). The raw RTL is hopeless without abstraction.
#   2. Sidecar but a SINGLE property → REJECTED at 27 state bits: the joint
#      design, even with all five timers concretized, is still > 20, and a
#      single cone gives one cluster so per-cluster cannot split it.
#      → per-cluster slicing is load-bearing on top of concretization.
#   3. Sidecar + FIVE disjoint properties → automatic per-cluster fires,
#      slices each detector into its own cluster (Circuit__cl0..cl4), and
#      all five reachability properties come back SATISFIED + non-vacuous
#      (32–64 state clusters — the concretized timer + FSM, NOT a
#      degenerate sliced-away cone), each over a distinct cluster automaton.
#
# Verdict shape = the R46-5 mechanism fixture (`mu X. (det || <>X)`); this
# fixture's purpose is the abstraction-composition mechanism on real RTL at
# K=5, not a deep property finding. Each timer escapes its concretized set
# at the threshold → an OOB sink, the shape the realize numericity-gate fix
# (PR #94) makes sound.
#
# The included `prim_assert.sv` resolves on the verify path because the
# sv2v staging tempdir is on the include search path (the same PR that adds
# this fixture).
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
OUT="$(mktemp -d -t mununu-r46-sysrst-XXXXXX)"
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

echo "=== R46-6 — sysrst_ctrl detect K=5 per-cluster × concretization (real OpenTitan RTL) ==="
echo "fixture: ${SRC}/sysrst_ctrl_detect.sv (vendored, pinned) × 5 via a hand-written harness"
echo

cd "${SRC}"

echo "--- (1) no sidecar: raw 172-bit design must be rejected at the cap ---"
sed 's#sidecar = "sysrst_harness.mununu.json"##' verify.toml > "${OUT}/nosidecar.toml"
if "${MUNUNU}" verify --base-dir "${SRC}" "${OUT}/nosidecar.toml" > "${OUT}/nosidecar.out" 2>&1; then
  echo "FAIL: raw design verified instead of hitting the cap" >&2; exit 1
fi
grep -qiE "state bits|max supported|StateSpaceOverflow|2\^20" "${OUT}/nosidecar.out" || {
  echo "FAIL: no-sidecar run did not report a cap overflow" >&2
  tail -5 "${OUT}/nosidecar.out" >&2; exit 1; }
echo "ok: raw RTL rejected — param-concretization is load-bearing"
echo

echo "--- (2) sidecar + single property: concretized joint still busts (per-cluster needed) ---"
sed -n '1,/\[\[properties\]\]/p' verify.toml > "${OUT}/single.toml"
cat >> "${OUT}/single.toml" <<'EOF'
name = "reach_det0"
formula = "mu X. (det0_o || (<> X))"
over = "Circuit"
EOF
if "${MUNUNU}" verify --base-dir "${SRC}" "${OUT}/single.toml" > "${OUT}/single.out" 2>&1; then
  echo "FAIL: single-property concretized design verified instead of busting" >&2; exit 1
fi
grep -qiE "state bits|max supported|StateSpaceOverflow|2\^20" "${OUT}/single.out" || {
  echo "FAIL: single-property run did not report a cap overflow" >&2
  tail -5 "${OUT}/single.out" >&2; exit 1; }
echo "ok: concretized joint still busts with one cone — per-cluster slicing is load-bearing too"
echo

echo "--- (3) sidecar + five disjoint properties: per-cluster × concretization compose ---"
"${MUNUNU}" verify --json verify.toml > "${OUT}/verify.json" 2>"${OUT}/verify.err" || {
  echo "FAIL: mununu verify exited non-zero" >&2
  tail -15 "${OUT}/verify.err" >&2; exit 1; }

python3 - "${OUT}/verify.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))

routing = None
for s in r.get("sources", []):
    routing = (s.get("partition_summary") or {}).get("cluster_routing")
    if routing:
        break
assert routing, "FAIL: no cluster_routing — per-cluster verification did not fire"
clusters = sorted(set(routing.values()))
assert len(clusters) >= 5, f"FAIL: expected >=5 cluster automata, got {clusters}"
print(f"per-cluster: routed {len(routing)} properties to {len(clusters)} clusters {clusters}")

verds = {v["name"]: v for v in r.get("property_verdicts", [])}
assert verds, "FAIL: no property verdicts"
overs = set()
for name in ("reach_det0", "reach_det1", "reach_det2", "reach_det3", "reach_det4"):
    v = verds.get(name)
    assert v, f"FAIL: missing verdict {name}"
    assert (v["outcome"] == "holds"), f"FAIL: {name} not SATISFIED"
    assert v["total_states"] >= 16, \
        f"FAIL: {name} cluster has only {v['total_states']} states — timer was sliced away, not concretized"
    assert v["over"].startswith("Circuit__cl"), \
        f"FAIL: {name} not routed to a cluster automaton (over={v['over']})"
    overs.add(v["over"])
    print(f"verdict {name}: SAT {v['satisfying_states']}/{v['total_states']} over {v['over']}")
assert len(overs) >= 5, f"FAIL: properties did not route to 5 distinct clusters: {overs}"
print("R46-6 K=5 done-criteria met: real OpenTitan sysrst_ctrl detect blocks — "
      "raw RTL rejected, concretized joint still busts, per-cluster × "
      "param-concretization compose to fit all five detectors, all SATISFIED "
      "over distinct clusters on non-degenerate (concretized) cones.")
PY

echo
echo "=== R46-6 (sysrst_ctrl detect K=5) VALIDATION PASSED ==="
