#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the
# uart_codesign_protocol_spec example.
#
# Drives `mununu verify` against verify.toml; the firmware-side source
# uses the c-codesign adapter (requires clang on PATH), the peripheral
# is hand-authored CTXDSL.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/uart_codesign_protocol_spec"
TRANSCRIPT="$DIR/transcript.txt"

if ! command -v clang >/dev/null 2>&1; then
    echo "clang not found on \$PATH" 1>&2
    echo "install via xcode-select --install (macOS) or apt install clang (Linux)" 1>&2
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
    printf '# uart_codesign_protocol_spec — verify framework transcript\n'
    printf '# Regenerated via examples/verify/uart_codesign_protocol_spec/validate.sh\n'
    printf '#\n'
    printf '# C firmware (adapter = c-codesign, shells out to clang for LLVM IR\n'
    printf '# extraction) + hand-authored CTXDSL peripheral protocol spec.\n'
    printf '# Direct alphabet binding on the rendezvous-label alphabet derived\n'
    printf '# from the register-map sidecar. Asynchronous composition (Doc C\n'
    printf '# §C.5 — bus arbitration is non-deterministic, synchronous coupling\n'
    printf '# is unsound for racy access).\n\n'

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
    src = v["formula_source"]["kind"]
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
