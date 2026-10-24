#!/usr/bin/env bash
# validate.sh — R46-6 / GAP-2 on REAL OpenTitan RTL: per-cluster slicing ×
# param-concretization compose to verify a design neither primitive can fit
# alone.
#
# Fixture: two independent `pattgen_chan` instances (vendored OpenTitan,
# pinned in source/UPSTREAM_COMMIT.txt) wrapped by a hand-written harness
# that narrows the 116-bit ctrl_i struct to a small constant config + a free
# per-channel enable. Each channel carries the channel's wide state
# (prediv_q[32], data_q[64], clk_cnt_q[32], reps_q/rep_cnt_q[10], …).
#
# Contract, in three checks:
#   1. NO sidecar → REJECTED at 338 raw state bits (2 × 169). The raw real
#      RTL is hopeless without abstraction.
#   2. Sidecar but a SINGLE property → REJECTED at 28 state bits: the joint
#      design, even with every wide cell concretized, is still 2 × 14 > 20,
#      and a single cone gives one cluster so per-cluster cannot split it.
#      → per-cluster slicing is load-bearing, not just concretization.
#   3. Sidecar + TWO disjoint properties → automatic per-cluster fires,
#      slices each channel into its own cluster (14 effective bits each),
#      and both reachability properties come back SATISFIED + non-vacuous
#      (128-state clusters — i.e. the concretized wide cells, NOT a
#      degenerate sliced-away cone), each over a distinct cluster automaton.
#
# The reachability verdict is the same shape as the R46-5 mechanism fixture
# (`mu X. (done || <>X)`); this fixture's purpose is the abstraction-
# composition mechanism on real RTL, not a deep property finding. Each
# channel's counter escapes its concretized set → an OOB sink (the 129th
# state), the shape the realize numericity-gate fix (PR #94) makes sound.
#
# Like the other R46 fixtures, the BTOR2 is produced in mununu's own temp
# dir, so nothing cap-busting lands under examples/.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUNUNU="${MUNUNU:-${THIS_DIR}/../../../target/debug/mununu}"
SRC="${THIS_DIR}/source"
OUT="$(mktemp -d -t mununu-r46-pattgen-XXXXXX)"
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

echo "=== R46-6 — pattgen per-cluster × param-concretization (real OpenTitan RTL) ==="
echo "fixture: ${SRC}/pattgen_chan.sv (vendored, pinned) + hand-written 2-channel harness"
echo

cd "${SRC}"

# Check 1 — raw real RTL must be rejected (concretization load-bearing).
echo "--- (1) no sidecar: raw 338-bit design must be rejected at the cap ---"
sed 's#sidecar = "pattgen_harness.mununu.json"##' verify.toml > "${OUT}/nosidecar.toml"
if "${MUNUNU}" verify --base-dir "${SRC}" "${OUT}/nosidecar.toml" > "${OUT}/nosidecar.out" 2>&1; then
  echo "FAIL: raw design verified instead of hitting the cap" >&2
  exit 1
fi
grep -qiE "state bits|max supported|StateSpaceOverflow|2\^20" "${OUT}/nosidecar.out" || {
  echo "FAIL: no-sidecar run did not report a state-bit cap overflow" >&2
  tail -5 "${OUT}/nosidecar.out" >&2; exit 1; }
echo "ok: raw RTL rejected — param-concretization is load-bearing"
echo

# Check 2 — concretized joint, single property, must still be rejected
# (per-cluster slicing load-bearing on top of concretization).
echo "--- (2) sidecar + single property: concretized joint still busts (per-cluster needed) ---"
sed -n '1,/\[\[properties\]\]/p' verify.toml > "${OUT}/single.toml"
cat >> "${OUT}/single.toml" <<'EOF'
name = "reach_done0"
formula = "mu X. (done0_o || (<> X))"
over = "Circuit"
EOF
if "${MUNUNU}" verify --base-dir "${SRC}" "${OUT}/single.toml" > "${OUT}/single.out" 2>&1; then
  echo "FAIL: single-property concretized design verified instead of busting" >&2
  exit 1
fi
grep -qiE "state bits|max supported|StateSpaceOverflow|2\^20" "${OUT}/single.out" || {
  echo "FAIL: single-property run did not report a cap overflow" >&2
  tail -5 "${OUT}/single.out" >&2; exit 1; }
echo "ok: concretized joint still busts with one cone — per-cluster slicing is load-bearing too"
echo

# Check 3 — both primitives compose: per-cluster + concretization → all SAT.
echo "--- (3) sidecar + two disjoint properties: per-cluster × concretization compose ---"
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
assert len(clusters) >= 2, f"FAIL: expected >=2 cluster automata, got {clusters}"
print(f"per-cluster: routed {len(routing)} properties to {len(clusters)} clusters {clusters}")

verds = {v["name"]: v for v in r.get("property_verdicts", [])}
assert verds, "FAIL: no property verdicts"
overs = set()
for name in ("reach_done0", "reach_done1"):
    v = verds.get(name)
    assert v, f"FAIL: missing verdict {name}"
    assert (v["outcome"] == "holds"), f"FAIL: {name} not SATISFIED"
    # Non-vacuous: the cluster must carry the concretized wide cells, i.e.
    # many states (2^7 = 128 real + OOB), not a degenerate sliced-away cone.
    assert v["total_states"] >= 64, \
        f"FAIL: {name} cluster has only {v['total_states']} states — wide cells were sliced away, not concretized"
    assert v["over"].startswith("Circuit__cl"), \
        f"FAIL: {name} not routed to a cluster automaton (over={v['over']})"
    overs.add(v["over"])
    print(f"verdict {name}: SAT {v['satisfying_states']}/{v['total_states']} over {v['over']}")
assert len(overs) >= 2, f"FAIL: properties did not route to distinct clusters: {overs}"
print("R46-6 done-criteria met: real OpenTitan pattgen channels — raw RTL "
      "rejected, concretized joint still busts, per-cluster × "
      "param-concretization compose to fit each channel, both SATISFIED "
      "over distinct clusters on non-degenerate (concretized) cones.")
PY

echo
echo "=== R46-6 (pattgen per-cluster × concretization) VALIDATION PASSED ==="
