#!/usr/bin/env bash
# Roadmap C4 — the tracker and `main` must agree.
#
# An open issue that a commit on `main` or a merged pull request claims to close is a
# disagreement: either the fix is in and the issue should be closed, or the claim was wrong and
# the investigation was retired while the defect is still there. This prints every such issue
# and exits non-zero when there is one.
#
#   scripts/tracker_audit.sh            audit every open issue
#   scripts/tracker_audit.sh 553 548    audit these numbers (open or not)
#   scripts/tracker_audit.sh --control  the negative control: a closed issue with a known
#                                       closing PR must be reported, else the audit is vacuous
#
# Two sources, because this repository squash-merges and the squash commit carries the PR title,
# not its body: closing keywords in commit messages on `main` (the pre-2026-10 habit,
# `closes mununu#N`), and closing keywords in merged PR bodies (what GitHub acts on).
set -euo pipefail

KEYWORD='(close[sd]?|fixe?[sd]?|resolve[sd]?)[: ]+(mununu|Mumunu-team/mununu)?#'

claims_for() {
  local n="$1" re="${KEYWORD}${n}([^0-9]|\$)"
  git log main --format='  commit %h %s' -i -E --grep="$re" | head -3
  gh pr list --state merged --limit 50 --search "\"#$n\" in:body" --json number,title,body \
    --jq ".[] | select(.body | test(\"${KEYWORD}${n}([^0-9]|\$)\"; \"i\")) | \"  PR #\\(.number) \\(.title)\"" \
    2>/dev/null | head -3
}

audit() {
  local bad=0 n claims
  for n in "$@"; do
    claims=$(claims_for "$n")
    if [ -n "$claims" ]; then
      echo "issue #$n is claimed closed:"
      echo "$claims"
      bad=1
    fi
  done
  return $bad
}

if [ "${1:-}" = "--control" ]; then
  # mununu#878 was closed by PR #879, whose body says "Fixes #878". If the audit cannot see
  # that, it cannot see anything, and a green run would mean nothing.
  if audit 878 >/dev/null; then
    echo "tracker-audit: CONTROL FAILED — the audit did not report #878, which PR #879 closes" >&2
    exit 2
  fi
  echo "tracker-audit: control ok (#878 is reported as claimed closed)"
  exit 0
fi

numbers=()
if [ $# -gt 0 ]; then
  numbers=("$@")
else
  # No `mapfile`: macOS ships bash 3.2.
  while IFS= read -r n; do
    [ -n "$n" ] && numbers+=("$n")
  done < <(gh issue list --state open --limit 500 --json number --jq '.[].number')
fi

if [ ${#numbers[@]} -eq 0 ]; then
  echo "tracker-audit: no open issues"
  exit 0
fi
if audit "${numbers[@]}"; then
  echo "tracker-audit: none of the ${#numbers[@]} issues checked is claimed closed on main or in a merged PR"
else
  exit 1
fi
