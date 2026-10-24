#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the microprogram_plus_sv
# example.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/microprogram_plus_sv"
TRANSCRIPT="$DIR/transcript.txt"

# yosys + sv2v are required: the peripheral source is elaborated via
# the sv-yosys (sv2v -> Yosys -> BTOR2 -> KMTS) pipeline. They are not
# bundled with the mununu repo; the dev container installs them.
for tool in yosys sv2v; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "validate.sh: required tool '${tool}' not on PATH" >&2
    exit 2
  fi
done

echo "build: mununu binary (cargo)" 1>&2
cargo build --quiet --bin mununu

strip_logs() {
    perl -pe '
        s/\e\[[0-9;]*m//g;
        s/^\s*\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+//;
    ' | grep -v 'Logging initialized'
}

{
    printf '# microprogram_plus_sv — verify framework transcript\n'
    printf '# Regenerated via examples/verify/microprogram_plus_sv/validate.sh\n'
    printf '#\n'
    printf '# Hand-authored CTXDSL microcode (Microprogram) + real SV\n'
    printf '# peripheral elaborated via the sv-yosys (sv2v -> Yosys -> BTOR2\n'
    printf '# -> KMTS) pipeline; its single automaton is named Circuit.\n'
    printf '# Disjoint label alphabets → asynchronous composition = product of\n'
    printf '# the independent state spaces (4 microcode states × 4 peripheral\n'
    printf '# states = 16 composed states).\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== verdict shape (from --json output) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" --json 2>&1 \
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
    line += " (" + str(v["satisfying_states"]) + "/" + str(v["total_states"]) + " states)"
    line += " [over = " + v["over"] + "]"
    print(line)
'
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
