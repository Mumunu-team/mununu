#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the crewai_handoff example.
#
# Runs `mununu verify` against verify.toml in this directory and
# captures the human-readable transcript at transcript.txt. The
# script is byte-deterministic: re-running against the same commit
# must reproduce the same output (modulo wall-clock timestamps which
# are stripped).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/crewai_handoff"
TRANSCRIPT="$DIR/transcript.txt"

echo "build: mununu binary (cargo)" 1>&2
cargo build --quiet --bin mununu

strip_logs() {
    perl -pe '
        s/\e\[[0-9;]*m//g;
        s/^\s*\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+//;
    ' | grep -v 'Logging initialized'
}

{
    printf '# crewai_handoff — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/crewai_handoff/validate.sh\n'
    printf '#\n'
    printf '# Sequential 2-agent CrewAI crew; native `CrewaiAdapter` dispatch;\n'
    printf '# direct alphabet binding; `crew.*` wildcard composition member\n'
    printf '# (expands to all three emitted automata); per-automaton\n'
    printf '# `reachable` properties.\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify --json (subset: project + verdict shape) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" --json 2>&1 \
        | strip_logs \
        | python3 -c '
import json, sys
report = json.load(sys.stdin)
print("project = " + report["project"])
print("composition.name = " + report["composition"]["name"])
print("composition.semantics = " + report["composition"]["semantics"])
for v in report["property_verdicts"]:
    src = v["formula_source"]["kind"]
    sat = (v["outcome"] == "holds")
    print("  property `" + v["name"] + "`: satisfied = " + str(sat) + ", source = " + src)
'
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
