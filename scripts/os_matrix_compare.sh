#!/usr/bin/env bash
# Roadmap I1 — the three OS lanes of .github/workflows/os-matrix.yml must have RUN the same
# tests. Each lane's `scripts/test_count_gate.sh` line ("test-count-gate: N tests passed (floor
# F)") is uploaded as count-<os>.txt; this script reads them all and fails when the spread
# between the largest and the smallest count exceeds the documented allowance — the number of
# tests listed in measurements/os-matrix-unix-only-tests.txt, one per line, the only tests that
# may legitimately exist on one OS and not another. A lane that ran fewer tests and still went
# green is C3's bug in a new coat.
#
#   scripts/os_matrix_compare.sh <count-file>...   compare (two or more files)
#   scripts/os_matrix_compare.sh --control         the negative control: unequal counts must fail
#                                                  at allowance 0 and pass at allowance 1, a file
#                                                  with no count must fail, else the check is vacuous
set -euo pipefail
ALLOW_FILE="$(dirname "$0")/../measurements/os-matrix-unix-only-tests.txt"

allowance() {
  # The number of non-blank, non-comment lines; `grep -c` exits 1 when that is 0.
  grep -cvE '^[[:space:]]*(#|$)' "$1" || true
}

compare() {
  local allow="$1"; shift
  local min="" max="" n f
  [ "$#" -ge 2 ] || { echo "os-matrix-compare: need at least two count files, got $#" >&2; return 1; }
  for f in "$@"; do
    n=$(sed -nE 's/.*test-count-gate: ([0-9]+) tests passed.*/\1/p' "$f" | tail -1)
    if [ -z "$n" ]; then
      echo "os-matrix-compare: $f carries no test count (the lane's gate did not report)" >&2
      return 1
    fi
    echo "$(basename "$f" .txt): $n tests"
    [ -z "$min" ] || [ "$n" -lt "$min" ] && min="$n"
    [ -z "$max" ] || [ "$n" -gt "$max" ] && max="$n"
  done
  local spread=$((max - min))
  if [ "$spread" -gt "$allow" ]; then
    echo "os-matrix-compare: the OS lanes ran different test sets — spread $spread, allowance $allow (measurements/os-matrix-unix-only-tests.txt). A test that exists on one OS only is listed there by name with its reason, or made to run everywhere; it is never cfg-skipped to go green." >&2
    return 1
  fi
  echo "os-matrix-compare: spread $spread within the allowance of $allow"
}

if [ "${1:-}" = "--control" ]; then
  tmp=$(mktemp -d)
  printf 'test-count-gate: 100 tests passed (floor 50)\n' > "$tmp/a.txt"
  printf 'test-count-gate: 100 tests passed (floor 50)\n' > "$tmp/b.txt"
  printf 'test-count-gate: 99 tests passed (floor 50)\n' > "$tmp/c.txt"
  printf 'nothing here\n' > "$tmp/d.txt"
  ok=1
  compare 0 "$tmp/a.txt" "$tmp/b.txt" >/dev/null 2>&1 || { echo "CONTROL FAILED: equal counts did not pass" >&2; ok=0; }
  compare 0 "$tmp/a.txt" "$tmp/c.txt" >/dev/null 2>&1 && { echo "CONTROL FAILED: a spread of 1 passed at allowance 0" >&2; ok=0; }
  compare 1 "$tmp/a.txt" "$tmp/c.txt" >/dev/null 2>&1 || { echo "CONTROL FAILED: a spread of 1 did not pass at allowance 1" >&2; ok=0; }
  compare 0 "$tmp/a.txt" "$tmp/d.txt" >/dev/null 2>&1 && { echo "CONTROL FAILED: a file with no count passed" >&2; ok=0; }
  compare 0 "$tmp/a.txt" >/dev/null 2>&1 && { echo "CONTROL FAILED: a single file passed" >&2; ok=0; }
  rm -rf "$tmp"
  [ "$ok" = 1 ] || exit 2
  echo "os-matrix-compare: control ok"
  exit 0
fi

compare "$(allowance "$ALLOW_FILE")" "$@"
