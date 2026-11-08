#!/usr/bin/env bash
# Roadmap I2 — the smoke test every release archive passes ON ITS OWN OS before it is packaged
# (.github/workflows/release.yml), and the one a self-hosted install can run from docs/release.md.
#
#   scripts/release_smoke.sh <path-to-mununu> [<path-to-mununu-extract>]
#
# It proves four things, each a line of output: the binary starts and links its libraries
# (`--version` loads libz3 on every OS); the licence surface answers in every state (`missing`
# here, exit 0); the CTXDSL front end and the μ-calculus engines run (a summary of the counters
# example); the BTOR2 verb and the engine portfolio decide a tiny design (twocount2c, which
# reaches `bad`). A smoke that does not run the engines would have passed on an archive that
# carried the wrong binary — which is what the release workflow shipped before I2.
set -euo pipefail
M="${1:?usage: release_smoke.sh <mununu> [<mununu-extract>]}"
X="${2:-}"
cd "$(dirname "$0")/.."

echo "== version"
"$M" --version
echo "== licence status (missing is the expected state on a fresh host)"
out=$("$M" --quiet licence status)
echo "$out"
grep -q '^licence: ' <<<"$out"
echo "== CTXDSL: context summarize examples/counters/counters.ctxdsl"
"$M" --quiet context summarize examples/counters/counters.ctxdsl | head -5
echo "== BTOR2: btor2 verify examples/btor2/btor2tools_suite/twocount2c.btor2 (JSON summary)"
# The verb is a CI gate: exit 0 = holds / unknown, exit 2 = violated (twocount2c reaches `bad`,
# so 2 is the expected code here); anything else is the binary failing, not a verdict.
set +e
out=$("$M" --quiet btor2 verify examples/btor2/btor2tools_suite/twocount2c.btor2)
rc=$?
set -e
echo "$out" | head -c 400; echo
case "$rc" in 0|2) ;; *) echo "release-smoke: btor2 verify exited $rc (not a verdict code)" >&2; exit 1 ;; esac
grep -qE '"outcome"[[:space:]]*:[[:space:]]*"(violated|holds|unknown|bottom|unreachable|reachable)' <<<"$out" \
  || { echo "release-smoke: the BTOR2 verb printed no verdict" >&2; exit 1; }
# `native` is the in-process BMC / k-induction engine over Z3: its appearing among the deciders
# proves libz3 was not only loaded at start but ran a query on this OS.
grep -q '"native"' <<<"$out" \
  || { echo "release-smoke: the native (Z3) engine reported no conclusion — libz3 did not run" >&2; exit 1; }
if [ -n "$X" ]; then
  # mununu-extract has subcommands and no `--version`; `--help` is its "I start" line.
  echo "== mununu-extract --help (first line)"
  "$X" --help | head -1
fi
echo "release-smoke: ok"
