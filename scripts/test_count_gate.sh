#!/usr/bin/env bash
# Roadmap C3 — a test lane that reports PASS having run no tests is an error, not a pass.
#
# Reads a `cargo test` or `cargo nextest run` log and checks the number of tests that RAN
# against a committed floor. The C1 incident is the class: `cargo test --lib api` resolved to
# a 9-line crate with zero tests and reported success for weeks. A green that executed nothing
# must read as red.
#
#   scripts/test_count_gate.sh <log>            check the log against measurements/test-count-floor.txt
#   scripts/test_count_gate.sh <log> <floor>    check against an explicit floor
#   scripts/test_count_gate.sh --control        the negative control: a log with no summary, a log
#                                               with zero tests and a log below the floor must each
#                                               fail, else the gate is vacuous
#
# Counting: cargo prints one `test result: … N passed; M failed; …` line per test binary (and per
# doctest set); they are summed. nextest prints one `Summary [ … ] X/Y tests run: P passed, …`
# line; `P passed` is taken. A log with neither line is a lane that produced no summary at all.
set -euo pipefail

FLOOR_FILE="$(dirname "$0")/../measurements/test-count-floor.txt"

count_passed() {
  local log="$1"
  if grep -qE 'Summary \[' "$log"; then
    grep -E 'Summary \[' "$log" | tail -1 | sed -E 's/.* ([0-9]+) passed.*/\1/'
  elif grep -qE '^test result:' "$log"; then
    grep -E '^test result:' "$log" | sed -E 's/.* ([0-9]+) passed.*/\1/' | awk '{s+=$1} END{print s+0}'
  else
    echo "NONE"
  fi
}

check() {
  local log="$1" floor="$2" n
  n=$(count_passed "$log")
  if [ "$n" = "NONE" ]; then
    echo "test-count-gate: the log carries no test summary at all — the lane ran nothing it could report" >&2
    return 1
  fi
  if [ "$n" -eq 0 ]; then
    echo "test-count-gate: 0 tests passed — a lane that executes no assertion cannot be green" >&2
    return 1
  fi
  if [ "$n" -lt "$floor" ]; then
    echo "test-count-gate: $n tests passed, below the floor of $floor (measurements/test-count-floor.txt). A lane vanished, or tests were removed on purpose: if the latter, lower the floor in the same commit and say why." >&2
    return 1
  fi
  echo "test-count-gate: $n tests passed (floor $floor)"
}

if [ "${1:-}" = "--control" ]; then
  tmp=$(mktemp -d)
  printf 'warning: nothing here\n' > "$tmp/empty.log"
  printf 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n' > "$tmp/zero.log"
  printf 'test result: ok. 3 passed; 0 failed; 0 ignored\ntest result: ok. 4 passed; 0 failed; 0 ignored\n' > "$tmp/low.log"
  printf '     Summary [  81.229s] 3628/3656 tests run: 3627 passed, 1 failed, 144 skipped\n' > "$tmp/nextest.log"
  ok=1
  check "$tmp/empty.log" 10 >/dev/null 2>&1 && { echo "CONTROL FAILED: a log with no summary passed" >&2; ok=0; }
  check "$tmp/zero.log" 10 >/dev/null 2>&1 && { echo "CONTROL FAILED: a zero-test log passed" >&2; ok=0; }
  check "$tmp/low.log" 10 >/dev/null 2>&1 && { echo "CONTROL FAILED: 7 tests passed a floor of 10" >&2; ok=0; }
  check "$tmp/low.log" 7 >/dev/null 2>&1 || { echo "CONTROL FAILED: 7 tests did not pass a floor of 7" >&2; ok=0; }
  check "$tmp/nextest.log" 3600 >/dev/null 2>&1 || { echo "CONTROL FAILED: the nextest summary was not read" >&2; ok=0; }
  rm -rf "$tmp"
  [ "$ok" = 1 ] || exit 2
  echo "test-count-gate: control ok (no-summary, zero and below-floor logs fail; at-floor and nextest logs pass)"
  exit 0
fi

log="${1:?usage: test_count_gate.sh <log> [floor] | --control}"
floor="${2:-$(grep -vE '^\s*#|^\s*$' "$FLOOR_FILE" | head -1)}"
check "$log" "$floor"
