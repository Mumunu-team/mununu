#!/usr/bin/env bash
# validate.sh — V.3 speculative non-interference (vulnerable vs safe). §Phase 7
# domain validation; controlling doc docs/design/industrial-value-and-validation-domains.md §6.
#
# For each design:
#   1. Re-generate into a tmp dir and diff against the checked-in vulnerable/
#      safe/ (the generator is deterministic).
#   2. Run `mununu verify` and assert the verdicts:
#        deadlock_freedom = true  (both)
#        noninterference  = true under safe / false under vulnerable
#        leak_reachable   = false under safe / true under vulnerable
#      noninterference (= never(Leak) on the self-composed product) is the
#      discriminating contract-conformance verdict; leak_reachable (its dual,
#      = reachable(Leak)) is the non-vacuity witness.
#
# Non-interference is a 2-safety hyperproperty verified by SELF-COMPOSITION:
# two copies run with the same public input but independent secrets; the
# property reduces to ordinary safety on the product (never(Leak)). The model
# is a generated design-pattern demonstration of the speculative-non-interference
# domain — an abstract speculation→cache side-channel model, NOT an RTL pipeline
# and NOT a production Spectre checker (research-grade; claims-integrity).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v3_specsafe_pipeline"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: cargo build -p mununu-cli" >&2
  exit 2
fi

# assert_verdicts <model> <expected noninterference: true|false>
assert_verdicts() {
  local model="$1" expect_ni="$2"
  local jf
  jf="$(mktemp)"
  "${MUNUNU}" verify --fail-on none "${DIR}/${model}/verify.toml" --json 2>/dev/null >"${jf}"
  python3 -c '
import json, sys
model, expect_ni = sys.argv[2], sys.argv[3]
r = json.load(open(sys.argv[1]))
got = {v["name"]: (v["outcome"] == "holds") for v in r["property_verdicts"]}
ni = (expect_ni == "true")
want = {
    "deadlock_freedom": True,
    "noninterference": ni,
    "leak_reachable": (not ni),   # dual of noninterference
}
ok = True
for name, exp in want.items():
    g = got.get(name)
    print("    {:<18} {}  (expected {})".format(name + ":", g, exp))
    if g is not exp:
        ok = False
        sys.stderr.write("FAIL ({}): {} expected {}, got {}\n".format(model, name, exp, g))
if not ok:
    sys.exit(1)
' "${jf}" "${model}" "${expect_ni}"
  local rc=$?
  rm -f "${jf}"
  return $rc
}

echo "=== V.3 speculative non-interference — vulnerable vs safe ==="
for entry in "vulnerable false" "safe true"; do
  set -- $entry
  model="$1"; expect_ni="$2"
  echo "--- ${model} (expect noninterference=${expect_ni}) ---"
  TMP="$(mktemp -d)"
  python3 "${DIR}/generate.py" "${model}" "${TMP}" >/dev/null
  if ! diff -rq "${DIR}/${model}" "${TMP}" >/dev/null 2>&1; then
    echo "FAIL: checked-in ${model}/ differs from generate.py output — re-run the generator and commit." >&2
    diff -rq "${DIR}/${model}" "${TMP}" || true
    rm -rf "${TMP}"; exit 1
  fi
  rm -rf "${TMP}"
  assert_verdicts "${model}" "${expect_ni}"
done

echo
echo "=== V.3 VALIDATION PASSED ==="
echo "Self-composing the speculative-load side channel reduces non-interference (a"
echo "2-safety hyperproperty) to never(Leak): the vulnerable design leaks a secret-"
echo "dependent cache footprint (noninterference=false), the squashed-speculation"
echo "design does not (noninterference=true)."
