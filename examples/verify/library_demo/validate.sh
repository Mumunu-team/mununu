#!/usr/bin/env bash
# validate.sh — parameterised library templates demo (plan Part 6 item 7).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/library_demo"
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
    printf '# library_demo — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/library_demo/validate.sh\n'
    printf '#\n'
    printf '# Two shipped library templates (PLIC, watchdog) instantiated\n'
    printf '# 3× and 2× respectively via count=N. Five independent automata\n'
    printf '# from two source files. Plan Part 6 item 7.\n\n'

    printf '=== mununu library list ===\n'
    ./target/debug/mununu library list 2>&1 | strip_logs

    printf '\n=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
