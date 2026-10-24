#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the langgraph_workflow example.
#
# Runs `mununu verify` against verify.toml in this directory and
# captures the human-readable transcript at transcript.txt. The
# script is byte-deterministic: re-running against the same commit
# must reproduce the same output (modulo wall-clock timestamps which
# are stripped).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/langgraph_workflow"
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
    printf '# langgraph_workflow — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/langgraph_workflow/validate.sh\n'
    printf '#\n'
    printf '# Conditional-branching LangGraph (classify -> {billing, tech} -> done)\n'
    printf '# via the native `LangGraphAdapter`; direct alphabet binding; two\n'
    printf '# property templates (reachable + mutual_exclusion).\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify --json (subset: project + verdict shape) ===\n'
    ./target/debug/mununu verify "$DIR/verify.toml" --json 2>&1 \
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
