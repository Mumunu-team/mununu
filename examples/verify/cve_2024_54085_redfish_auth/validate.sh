#!/usr/bin/env bash
# validate.sh — Redfish auth-bypass pattern (CVE-2024-54085) reproduction.
#
# Regenerates transcript.txt by running mununu against both verify
# manifests (vulnerable + fixed). Demonstrates:
#   1. `mununu memory check` audits the declared abstraction posture.
#   2. `mununu verify` on the vulnerable manifest emits a counterexample
#      lasso witnessing the auth-bypass.
#   3. `mununu verify` on the fixed manifest satisfies the same safety
#      property; bypass-reachability fails as expected.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/cve_2024_54085_redfish_auth"
TRANSCRIPT="$DIR/transcript.txt"

echo "build: mununu binary (cargo)" 1>&2
cargo build --quiet --bin mununu --package mununu-cli

strip_logs() {
    perl -pe '
        s/\e\[[0-9;]*m//g;
        s/^\s*\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+//;
    ' | grep -v 'Logging initialized'
}

{
    printf '# cve_2024_54085_redfish_auth — verify-framework transcript\n'
    printf '# Regenerated via examples/verify/cve_2024_54085_redfish_auth/validate.sh\n'
    printf '#\n'
    printf '# Models the Redfish authentication-bypass pattern publicly\n'
    printf '# described in CVE-2024-54085. Two manifests:\n'
    printf '#   verify_vulnerable.toml — safety property fails with lasso\n'
    printf '#   verify_fixed.toml      — safety property holds; bypass unreachable\n'
    printf '#\n'
    printf '# Disclaimer: hand-authored model of the property class. NOT\n'
    printf '# extracted from any specific firmware binary. See README.md.\n\n'

    printf '=== mununu memory check (vulnerable) ===\n'
    ./target/debug/mununu memory check "$DIR/verify_vulnerable.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify (vulnerable) ===\n'
    ./target/debug/mununu verify --fail-on none --print-counterexample "$DIR/verify_vulnerable.toml" 2>&1 | strip_logs

    printf '\n=== mununu memory check (fixed) ===\n'
    ./target/debug/mununu memory check "$DIR/verify_fixed.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify (fixed) ===\n'
    ./target/debug/mununu verify --fail-on none --print-counterexample "$DIR/verify_fixed.toml" 2>&1 | strip_logs
} > "$TRANSCRIPT"

printf 'wrote transcript to %s (%d lines)\n' \
    "$TRANSCRIPT" "$(wc -l < "$TRANSCRIPT" | tr -d ' ')"
