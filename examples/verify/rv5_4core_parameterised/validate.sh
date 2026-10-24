#!/usr/bin/env bash
# validate.sh — parameterised-instance support demo (plan Part 6 item 6).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/rv5_4core_parameterised"
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
    printf '# rv5_4core_parameterised — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/rv5_4core_parameterised/validate.sh\n'
    printf '#\n'
    printf '# ONE source declaration (`count = 4`) expands to four\n'
    printf '# independent per-core pipeline automata. 3^4 = 81 reachable\n'
    printf '# composed states. Plan Part 6 item 6.\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify --print-alphabet (introspection) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" --print-alphabet 2>&1 | strip_logs
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
