#!/usr/bin/env bash
# validate.sh — industrial DMA-engine microcode + memory + IRQ
# controller composition (plan Part 6 item 5d).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/dma_engine_microcode"
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
    printf '# dma_engine_microcode — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/dma_engine_microcode/validate.sh\n'
    printf '#\n'
    printf '# DMA channel microcode (extracted from JSON via the v1 microcode\n'
    printf '# adapter) composed with a tracked-memory automaton and an IRQ\n'
    printf '# controller stub. Industrial-canonical DMA safety properties.\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify --print-alphabet (introspection of composed system) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" --print-alphabet 2>&1 | strip_logs
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
