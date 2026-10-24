#!/usr/bin/env bash
# validate.sh — V.2 store-buffering litmus (TSO vs SC). §Phase 7 domain
# validation; controlling doc docs/design/industrial-value-and-validation-domains.md §5.
#
# For each memory model:
#   1. Re-generate into a tmp dir and diff against the checked-in tso/ sc/
#      (the generator is deterministic).
#   2. Run `mununu verify` and assert the discriminating litmus verdict:
#        TSO ⇒ both_read_zero_reachable = true   (the relaxation is observable)
#        SC  ⇒ both_read_zero_reachable = false   (SC / the fence forbids it)
#      and other_outcome_reachable = true in both.
#
# This is the canonical SB result (herd / rmem / any MCM textbook). The
# model is a generated design-pattern demonstration of the memory-
# consistency domain, NOT a finding about a real processor (claims-integrity).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v2_tso_storebuffer"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: cargo build -p mununu-cli" >&2
  exit 2
fi

# assert_litmus <model> <expected both_read_zero: true|false>
assert_litmus() {
  local model="$1" expect_bz="$2"
  local jf
  jf="$(mktemp)"
  "${MUNUNU}" verify --fail-on none "${DIR}/${model}/verify.toml" --json 2>/dev/null >"${jf}"
  python3 -c '
import json, sys
model, expect_bz = sys.argv[2], sys.argv[3]
r = json.load(open(sys.argv[1]))
got = {v["name"]: (v["outcome"] == "holds") for v in r["property_verdicts"]}
bz = got.get("both_read_zero_reachable")
oo = got.get("other_outcome_reachable")
want_bz = (expect_bz == "true")
print("    both_read_zero_reachable: {}  (expected {})".format(bz, want_bz))
print("    other_outcome_reachable:  {}".format(oo))
if bz is not want_bz:
    sys.stderr.write("FAIL ({}): both_read_zero expected {}, got {}\n".format(model, want_bz, bz)); sys.exit(1)
if oo is not True:
    sys.stderr.write("FAIL ({}): other_outcome_reachable expected true, got {}\n".format(model, oo)); sys.exit(1)
' "${jf}" "${model}" "${expect_bz}"
  local rc=$?
  rm -f "${jf}"
  return $rc
}

echo "=== V.2 store-buffering litmus — TSO vs SC ==="
for entry in "tso true" "sc false"; do
  set -- $entry
  model="$1"; expect_bz="$2"
  echo "--- ${model} (expect both_read_zero=${expect_bz}) ---"
  TMP="$(mktemp -d)"
  python3 "${DIR}/generate.py" "${model}" "${TMP}" >/dev/null
  if ! diff -rq "${DIR}/${model}" "${TMP}" >/dev/null 2>&1; then
    echo "FAIL: checked-in ${model}/ differs from generate.py output — re-run the generator and commit." >&2
    diff -rq "${DIR}/${model}" "${TMP}" || true
    rm -rf "${TMP}"; exit 1
  fi
  rm -rf "${TMP}"
  assert_litmus "${model}" "${expect_bz}"
done

echo
echo "=== V.2 VALIDATION PASSED ==="
echo "Store-buffering r0=r1=0 is reachable under TSO and forbidden under SC —"
echo "mununu reproduces the canonical SB litmus distinction."
