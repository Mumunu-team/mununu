#!/usr/bin/env bash
# mununu — verify that a defect reproducer actually reproduces the defect.
#
# The `Reproducers Before Fixes` rule in CLAUDE.md says a cause fix needs a test that FAILS on the
# parent commit and passes on this one. A regression test that passes on the parent is decoration:
# it locks in behaviour, it does not demonstrate a defect.
#
#   make verify-repro TEST=<substring> [REF=HEAD~1] [PKG=mununu-core]
#
# How it works, and the limitation stated up front: in Rust a unit test lives INSIDE the production
# file it tests (`mod tests`), so the test and the fix cannot be separated by path. So this script
# checks out REF into a scratch worktree and copies the test-BEARING files from HEAD over it. That
# isolates the fix in every other file. Two honest outcomes:
#
#   * it builds and the test FAILS  -> confirmed reproducer. This is what the rule wants.
#   * it builds and the test PASSES -> decoration, not a reproducer. The case most worth catching.
#   * it does not build             -> the test depends on API the fix introduced, so the check
#                                      cannot run. NOT a pass. Declare the evidence in the PR.
#
# Reach limit, measured against mununu#542 and #544 (the fixes that motivated the rule): BOTH came
# back INCONCLUSIVE, because each introduced API its own test uses. So this instrument reliably
# catches decoration and reliably refuses to bless anything else, but it does not mechanically
# confirm most API-introducing fixes. Raising the reach needs hunk-level granularity (apply only
# `mod tests` hunks), which is not written. Do not treat a green run as the rule's only enforcement.
#
# Never reports success on a test that passes at REF.
set -uo pipefail

TEST="${TEST:-}"
REF="${REF:-HEAD~1}"
PKG="${PKG:-mununu-core}"

if [ -z "$TEST" ]; then
  echo "usage: make verify-repro TEST=<test-name-substring> [REF=HEAD~1] [PKG=mununu-core]" >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel)" || exit 2
cd "$repo_root" || exit 2

if ! git rev-parse --verify --quiet "$REF" >/dev/null; then
  echo "verify-repro: REF '$REF' does not resolve" >&2
  exit 2
fi

# Files changed between REF and HEAD that mention the test name — the test-bearing files.
# Portable to bash 3.2 (macOS /bin/bash): no `mapfile`, no empty-array expansion under `set -u`.
bearing=""
while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  if grep -q -- "$TEST" "$f"; then bearing="$bearing $f"; fi
done <<EOF
$(git diff --name-only "$REF" HEAD -- '*.rs')
EOF
bearing="${bearing# }"

if [ -z "$bearing" ]; then
  cat >&2 <<MSG
verify-repro: no file changed between $REF and HEAD mentions '$TEST'.

Either the test predates $REF (then it is not this commit's reproducer), or the name is wrong.
A cause fix must introduce or change the test that demonstrates the defect.
MSG
  exit 1
fi

echo "verify-repro: test '$TEST'"
echo "  ref:             $REF"
echo "  test-bearing:    $bearing"

work="$(mktemp -d)/repro"
cleanup() { git worktree remove --force "$work" >/dev/null 2>&1 || true; }
trap cleanup EXIT

git worktree add --detach --quiet "$work" "$REF" || { echo "verify-repro: worktree failed" >&2; exit 2; }
for f in $bearing; do
  mkdir -p "$work/$(dirname "$f")"
  cp "$f" "$work/$f"
done

# Reuse the main target dir: a cold build here costs minutes. Per CLAUDE.md's hook-serialisation
# rule this is a HEAVY cargo workload — do not run it concurrently with a pre-commit hook.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target}"

echo "  building $PKG at $REF with the test files from HEAD..."
log="$(mktemp)"
if ! (cd "$work" && cargo test -p "$PKG" --lib --no-run) >"$log" 2>&1; then
  echo
  echo "INCONCLUSIVE — the test does not build against $REF."
  echo "  The test depends on API introduced by this commit, so the reproducer cannot be verified"
  echo "  mechanically. This is NOT a pass. State in the PR what evidence the defect rests on, and"
  echo "  use 'Repro: consumer-only' or 'Repro: none — containment only' if that is the truth."
  echo
  tail -25 "$log"
  exit 3
fi

echo "  running..."
if (cd "$work" && cargo test -p "$PKG" --lib -- "$TEST") >"$log" 2>&1; then
  echo
  echo "FAIL — '$TEST' PASSES at $REF, so it does not reproduce anything."
  echo "  This is a regression test, not a reproducer. It locks in behaviour; it does not show a"
  echo "  defect. Either write a test that fails at $REF, or declare the fix honestly:"
  echo "    Repro: consumer-only <who> — <why>"
  echo "    Repro: none — containment only     (and do NOT close the cause issue)"
  echo
  grep -E "^test .* \.\.\. (ok|FAILED)|^test result:" "$log" | tail -10
  exit 1
fi

echo
echo "CONFIRMED — '$TEST' fails at $REF and the fix is in this commit."
echo "  Commit trailer:  Repro: in-repo $TEST"
grep -E "^test .* \.\.\. (ok|FAILED)|^test result:|panicked at" "$log" | tail -10
exit 0
