#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the V.5 GALS 4-phase
# handshake demo.
#
# V.5 of the §Phase 7 domain-validation track. Controlling doc:
# `docs/design/industrial-value-and-validation-domains.md` §7.
#
# Runs `mununu verify` against verify.toml in this directory and
# captures the human-readable transcript at transcript.txt. The
# script is byte-deterministic: re-running against the same commit
# must reproduce the same output (modulo wall-clock timestamps which
# are stripped).
#
# Requires `LIBRARY_PATH=/usr/local/opt/z3/lib` (or equivalent z3
# install location) for the cargo build.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v5_gals_handshake"
TRANSCRIPT="$DIR/transcript.txt"

echo "build: mununu binary (cargo)" 1>&2
cargo build --quiet --bin mununu

strip_logs() {
    perl -pe '
        s/\e\[[0-9;]*m//g;
        s/^\s*\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+//;
    ' | grep -v 'Logging initialized'
}

summarise_json() {
    python3 -c '
import json, sys
report = json.load(sys.stdin)
print("project = " + report["project"])
print("composition.name = " + report["composition"]["name"])
print("composition.semantics = " + report["composition"]["semantics"])
print("composition.members = " + ", ".join(report["composition"]["members"]))
for v in report["property_verdicts"]:
    name = v["name"]
    sat = (v["outcome"] == "holds")
    print(f"property[{name}] satisfied = {sat}")
'
}

{
    printf '# v5_gals_handshake — V.5 GALS-domain demo transcript\n'
    printf '# Regenerated via examples/verify/v5_gals_handshake/validate.sh\n'
    printf '#\n'
    printf '# Two sub-fixtures (verify.toml + verify_mutex.toml) — both halves of the\n'
    printf '# §Phase 7 V.5 spec:\n'
    printf '#   verify.toml       : 4-phase REQ/ACK handshake (Sender + Receiver, sync compose)\n'
    printf '#   verify_mutex.toml : 3-input mutex arbiter (mutual exclusion safety)\n'
    printf '#\n'
    printf '# Sharp-everywhere semantics; expected verdicts (all SATISFIED):\n'
    printf '#   verify.toml       : deadlock_freedom, request_eventually_acked, recurrence_to_idle\n'
    printf '#   verify_mutex.toml : mutex_pairwise_exclusion, mutex_no_deadlock\n'
    printf '#\n'
    printf '# §Phase 7 V.5 done-criterion: all five verdicts true on the singular\n'
    printf '# pipeline; CADP cross-check is a manual follow-up.\n\n'

    printf '=== verify.toml — 4-phase handshake — human-readable ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== verify.toml — JSON verdict shape ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" --json 2>&1 \
        | strip_logs | summarise_json

    printf '\n=== verify_mutex.toml — 3-input mutex — human-readable ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify_mutex.toml" 2>&1 | strip_logs

    printf '\n=== verify_mutex.toml — JSON verdict shape ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify_mutex.toml" --json 2>&1 \
        | strip_logs | summarise_json
} > "$TRANSCRIPT"

echo "Transcript written to $TRANSCRIPT" 1>&2
echo "--- last 20 lines ---" 1>&2
tail -20 "$TRANSCRIPT" 1>&2
