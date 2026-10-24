#!/usr/bin/env bash
# validate.sh — SV multi-module composition under the verify framework.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/sv_multi_module"
TRANSCRIPT="$DIR/transcript.txt"

# yosys + sv2v are required: the submodules are elaborated and composed
# via the sv-yosys multi-module pipeline (sv2v -> Yosys -> BTOR2 -> KMTS).
# They are not bundled with the mununu repo; the dev container installs them.
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
    printf '# sv_multi_module — verify framework end-to-end transcript\n'
    printf '# Regenerated via examples/verify/sv_multi_module/validate.sh\n'
    printf '#\n'
    printf '# Producer + consumer SV modules composed via the sv-yosys\n'
    printf '# multi-module path: a top module instantiates both submodules,\n'
    printf '# each is lifted to a KMTS, and the shared `valid` net rendezvouses\n'
    printf '# across the synchronous composition (composed automaton = Circuit).\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
