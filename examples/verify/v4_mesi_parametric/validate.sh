#!/usr/bin/env bash
# validate.sh — V.4 parametric MESI (N=2,4,8). §Phase 7 domain-validation
# track; controlling doc docs/design/industrial-value-and-validation-domains.md §3.
#
# For each N ∈ {2,4,8}:
#   1. Re-generate the fixture into a tmp dir and diff against the checked-in
#      n<N>/ (the generator is deterministic — drift = a generator change that
#      wasn't re-committed).
#   2. Run `mununu verify` and assert all three verdicts are `true`:
#        coherence_safety, deadlock_freedom, write_visibility.
#
# Sharp-everywhere explicit async composition ⇒ definite 2-valued verdicts
# directly (no CEGAR / KleeneBot at this scale). The model is a generated
# design-pattern demonstration of the cache-coherence domain, NOT a finding
# about a real silicon protocol (claims-integrity).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v4_mesi_parametric"
MUNUNU="${MUNUNU:-./target/debug/mununu}"
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/local/opt/z3/lib}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: cargo build -p mununu-cli" >&2
  exit 2
fi

assert_verdicts() {
  local toml="$1"
  local jf
  jf="$(mktemp)"
  # `--json` writes the report to stdout; the INFO log line goes to stderr
  # (dropped here). Capture to a file so python reads JSON from argv, not
  # stdin (a `python3 - <<heredoc` would consume the heredoc as the script
  # and leave stdin empty).
  "${MUNUNU}" verify --fail-on none "${toml}" --json 2>/dev/null >"${jf}"
  python3 -c '
import json, sys
toml = sys.argv[2]
r = json.load(open(sys.argv[1]))
want = {"coherence_safety", "deadlock_freedom", "write_visibility"}
got = {v["name"]: (v["outcome"] == "holds") for v in r["property_verdicts"]}
missing = want - set(got)
if missing:
    sys.stderr.write("FAIL ({}): missing verdicts {}\n".format(toml, missing)); sys.exit(1)
bad = {k: got[k] for k in want if got[k] is not True}
for k in sorted(want):
    print("    {}: {}".format(k, got[k]))
if bad:
    sys.stderr.write("FAIL ({}): expected all true, got {}\n".format(toml, bad)); sys.exit(1)
' "${jf}" "${toml}"
  local rc=$?
  rm -f "${jf}"
  return $rc
}

echo "=== V.4 parametric MESI — N ∈ {2,4,8} ==="
for N in 2 4 8; do
  echo "--- N=${N} ---"
  TMP="$(mktemp -d)"
  python3 "${DIR}/generate.py" "${N}" "${TMP}" >/dev/null
  if ! diff -rq "${DIR}/n${N}" "${TMP}" >/dev/null 2>&1; then
    echo "FAIL: checked-in n${N}/ differs from generate.py output — re-run the generator and commit." >&2
    diff -rq "${DIR}/n${N}" "${TMP}" || true
    rm -rf "${TMP}"; exit 1
  fi
  rm -rf "${TMP}"
  assert_verdicts "${DIR}/n${N}/verify.toml"
done

echo
echo "=== V.4 VALIDATION PASSED ==="
echo "Coherence safety + deadlock-freedom + eventual write visibility hold on"
echo "the parametric MESI at N=2, 4, and 8 (generator deterministic; verdicts definite)."
