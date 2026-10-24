#!/usr/bin/env bash
# validate.sh — V.10 memory-fabric client-mux variant study.
#
# For each of the four issue-path variants:
#   1. Re-generate into a tmp dir and diff against the checked-in v1..v4/
#      (the generator is deterministic).
#   2. Run `mununu verify` and assert the discriminating verdicts.
#
# Then run the CONTROL: regenerate every variant with --strict-schedule (the
# arbiter may not grant while a request is in flight) and assert that v2's
# duplicate DISAPPEARS. That is what proves the duplicate is caused by the
# coincident grant+accept edge — backfill — and not by a modelling artifact.
# A model that reports the duplicate under both schedules is not modelling
# this bug.
#
# The model is a design-pattern demonstration of the issue-path question,
# cross-checked against monono's own measured hardware evidence (mem_board
# 10/12: 5,125 returns for 5,120 words; card B-21's 0.949 accepted beats per
# granted read slot). It is NOT itself a measurement of monono's RTL.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v10_mem_fabric_client_mux"
MUNUNU="${MUNUNU:-./target/debug/mununu}"

if [[ ! -x "${MUNUNU}" ]]; then
  echo "validate.sh: mununu not found at ${MUNUNU} — build with: cargo build -p mununu-cli" >&2
  exit 2
fi

# assert_variant <dir> <variant> <expect_dup> <expect_skip> <expect_wedgefree>
assert_variant() {
  local dir="$1" variant="$2" xdup="$3" xskip="$4" xwedge="$5"
  local jf
  jf="$(mktemp)"
  # Run from the repo root, not from inside the variant dir: mununu writes
  # logs/mununu.log relative to CWD, and a stray logs/ would break the
  # regenerate-and-diff check above. Source paths resolve relative to the toml.
  "${MUNUNU}" verify --fail-on none "${dir}/verify.toml" --json 2>/dev/null >"${jf}"
  python3 -c '
import json, sys
jf, variant, xdup, xskip, xwedge = sys.argv[1:6]
r = json.load(open(jf))
got = {v["name"]: (v["outcome"] == "holds") for v in r["property_verdicts"]}
want = {
    "duplicate_issue_reachable":  xdup   == "true",
    "skipped_word_reachable":     xskip  == "true",
    "clean_completion_reachable": True,      # the non-vacuity gate
    "never_wedged":               xwedge == "true",
}
print("    dup=%-5s skip=%-5s done=%-5s wedge_free=%s" % (
    got.get("duplicate_issue_reachable"), got.get("skipped_word_reachable"),
    got.get("clean_completion_reachable"), got.get("never_wedged")))
bad = [k for k, w in want.items() if got.get(k) is not w]
if bad:
    for k in bad:
        sys.stderr.write("FAIL (%s): %s expected %s, got %s\n"
                         % (variant, k, want[k], got.get(k)))
    sys.exit(1)
' "${jf}" "${variant}" "${xdup}" "${xskip}" "${xwedge}"
  local rc=$?
  rm -f "${jf}"
  return $rc
}

echo "=== V.10 mem_fabric client mux — four issue-path variants ==="
echo
echo "--- backfill schedule (the real arbiter: a grant is possible on every edge) ---"

# variant  dup    skip   wedge-free
for entry in \
    "v1 false false true" \
    "v2 true  false false" \
    "v3 false true  false" \
    "v4 false false true"
do
  set -- $entry
  variant="$1"; xdup="$2"; xskip="$3"; xwedge="$4"
  echo "  ${variant} (expect dup=${xdup} skip=${xskip} wedge_free=${xwedge})"

  TMP="$(mktemp -d)"
  python3 "${DIR}/generate.py" "${variant}" "${TMP}" >/dev/null
  if ! diff -rq "${DIR}/${variant}" "${TMP}/${variant}" >/dev/null 2>&1; then
    echo "FAIL: checked-in ${variant}/ differs from generate.py output — re-run the generator and commit." >&2
    diff -rq "${DIR}/${variant}" "${TMP}/${variant}" || true
    rm -rf "${TMP}"; exit 1
  fi
  rm -rf "${TMP}"

  assert_variant "${DIR}/${variant}" "${variant}" "${xdup}" "${xskip}" "${xwedge}"
done

echo
echo "--- CONTROL: strict schedule, no back-to-back grant ---"
echo "    v2's duplicate must DISAPPEAR — the bug is the coincident edge, not the register."
CTRL="$(mktemp -d)"
for variant in v1 v2 v3 v4; do
  python3 "${DIR}/generate.py" "${variant}" "${CTRL}" --strict-schedule >/dev/null
done
# Under a strict schedule nothing may duplicate; v3 still skips (its skip comes
# from a REFUSED grant, not from simultaneity — a different root cause, and the
# model separates them).
for entry in "v1 false false true" "v2 false false true" \
             "v3 false true false" "v4 false false true"
do
  set -- $entry
  variant="$1"; xdup="$2"; xskip="$3"; xwedge="$4"
  echo "  ${variant} strict (expect dup=${xdup} skip=${xskip})"
  assert_variant "${CTRL}/${variant}" "${variant}-strict" "${xdup}" "${xskip}" "${xwedge}"
done
rm -rf "${CTRL}"

echo
echo "=== V.10 VALIDATION PASSED ==="
echo "v1 sound; v2 duplicates (and only under backfill); v3 trades the duplicate"
echo "for a skipped word; v4 is sound and never wedges."
