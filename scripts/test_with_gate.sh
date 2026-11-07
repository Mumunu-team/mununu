#!/usr/bin/env bash
# Roadmap C3 — `make test` runs the workspace suite and then refuses to be green unless the
# suite's own summary says enough tests ran (scripts/test_count_gate.sh). The negative control
# runs first, so a gate that cannot fail fails this target before the suite starts.
#
# Runner choice is the Makefile's (unchanged): MUNUNU_TEST_RUNNER=nextest|cargo, else cargo on
# macOS and nextest where it is installed. nextest skips doctests, so they run separately there.
set -uo pipefail

CARGO="${CARGO:-cargo}"
HERE="$(cd "$(dirname "$0")" && pwd)"
LOG="${MUNUNU_TEST_LOG:-${CARGO_TARGET_DIR:-target}/make-test.log}"
mkdir -p "$(dirname "$LOG")"

"$HERE/test_count_gate.sh" --control || exit 2

runner="${MUNUNU_TEST_RUNNER:-}"
if [ -z "$runner" ]; then
  if [ "$(uname -s)" = "Darwin" ]; then runner=cargo
  elif command -v cargo-nextest >/dev/null 2>&1; then runner=nextest
  else runner=cargo; fi
fi

: > "$LOG"
if [ "$runner" = "nextest" ]; then
  "$CARGO" nextest run --workspace 2>&1 | tee -a "$LOG"
  status=${PIPESTATUS[0]}
  if [ "$status" -eq 0 ]; then
    "$CARGO" test --workspace --doc 2>&1 | tee -a "$LOG"
    status=${PIPESTATUS[0]}
  fi
else
  "$CARGO" test --workspace 2>&1 | tee -a "$LOG"
  status=${PIPESTATUS[0]}
fi
[ "$status" -eq 0 ] || exit "$status"

"$HERE/test_count_gate.sh" "$LOG"
