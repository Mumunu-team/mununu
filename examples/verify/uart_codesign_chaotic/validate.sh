#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the uart_codesign_chaotic
# example.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/uart_codesign_chaotic"
TRANSCRIPT="$DIR/transcript.txt"

if ! command -v clang >/dev/null 2>&1; then
    echo "clang not found on \$PATH" 1>&2
    exit 1
fi

echo "build: mununu binary (cargo)" 1>&2
cargo build --quiet --bin mununu

strip_logs() {
    perl -pe '
        s/\e\[[0-9;]*m//g;
        s/^\s*\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+//;
    ' | grep -v 'Logging initialized'
}

{
    printf '# uart_codesign_chaotic — verify framework transcript\n'
    printf '# Regenerated via examples/verify/uart_codesign_chaotic/validate.sh\n'
    printf '#\n'
    printf '# Canonical chaotic-stub codesign verification: C firmware\n'
    printf '# (adapter = c-codesign) composed asynchronously with a chaotic\n'
    printf '# peripheral stub (matches what codesign::coupling::emit_coupling_fragment\n'
    printf '# would generate from the register map). Doc C §C.5 — asynchronous\n'
    printf '# composition is mandatory for racy access.\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== verdict shape (from --json output) ===\n'
    ./target/debug/mununu verify "$DIR/verify.toml" --json 2>&1 \
        | strip_logs \
        | python3 -c '
import json, sys
report = json.load(sys.stdin)
print("project = " + report["project"])
print("composition.name = " + report["composition"]["name"])
print("composition.semantics = " + report["composition"]["semantics"])
print("composition.members = " + ", ".join(report["composition"]["members"]))
for v in report["property_verdicts"]:
    sat = (v["outcome"] == "holds")
    line = "  " + v["name"] + ": "
    line += "SATISFIED" if sat else "VIOLATED"
    line += " (" + str(v["satisfying_states"]) + "/" + str(v["total_states"]) + " states, "
    line += str(len(v["initial_satisfying"])) + "/" + str(len(v["initial_states"])) + " initial)"
    line += " [over = " + v["over"] + "]"
    print(line)
'
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
