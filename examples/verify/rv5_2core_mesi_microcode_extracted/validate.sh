#!/usr/bin/env bash
# validate.sh — parity fixture for the microcode adapter (plan Part 6 item 5c).
#
# Same scenario as ../rv5_2core_mesi_microprogram/, but the
# microprogram is extracted from JSON microcode via the v1 microcode
# adapter instead of hand-authored CTXDSL. Verdict equivalence with
# the hand-authored sibling proves the adapter preserves semantics.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/rv5_2core_mesi_microcode_extracted"
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
    printf '# rv5_2core_mesi_microcode_extracted — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/rv5_2core_mesi_microcode_extracted/validate.sh\n'
    printf '#\n'
    printf '# Same 2-core MESI + 3-step microprogram scenario as the hand-authored\n'
    printf '# rv5_2core_mesi_microprogram/ fixture; here the microprogram is\n'
    printf '# extracted from JSON microcode via the v1 microcode adapter.\n'
    printf '# Verdict equivalence proves the adapter preserves semantics.\n\n'

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
print("composition.members = " + ", ".join(report["composition"]["members"]))
for v in report["property_verdicts"]:
    src = v["formula_source"]["kind"]
    sat = (v["outcome"] == "holds")
    print("  property `" + v["name"] + "`: satisfied = " + str(sat) + ", source = " + src + ", states = " + str(v["satisfying_states"]) + "/" + str(v["total_states"]))
'
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
