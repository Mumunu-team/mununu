#!/usr/bin/env bash
# validate.sh — end-to-end smoke test for the V.0 minimal MESI demo.
#
# V.0 of the §Phase 7 domain-validation track. Controlling doc:
# `docs/design/industrial-value-and-validation-domains.md` §3.
#
# Runs `mununu verify` against verify.toml in this directory and
# captures the human-readable transcript at transcript.txt. The
# script is byte-deterministic: re-running against the same commit
# must reproduce the same output (modulo wall-clock timestamps which
# are stripped).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
DIR="examples/verify/v0_mesi_2agent"
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
    printf '# v0_mesi_2agent — V.0 minimal MESI demo transcript\n'
    printf '# Regenerated via examples/verify/v0_mesi_2agent/validate.sh\n'
    printf '#\n'
    printf '# 2 L1 caches, 1 tracked line, free environment driving.\n'
    printf '# Sharp-everywhere semantics; verdicts must be:\n'
    printf '#   - mesi_coherence_invariant  : satisfied = true\n'
    printf '#   - deadlock_freedom          : satisfied = true\n'
    printf '#\n'
    printf '# §Phase 7 V.0 done-criterion: both verdicts true on the singular pipeline.\n\n'

    printf '=== mununu verify (human-readable) ===\n'
    ./target/debug/mununu verify --fail-on none "$DIR/verify.toml" 2>&1 | strip_logs

    printf '\n=== mununu verify --json (verdict shape) ===\n'
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
    name = v["name"]
    sat = (v["outcome"] == "holds")
    print(f"property[{name}] satisfied = {sat}")
'
} > "$TRANSCRIPT"

echo "Transcript written to $TRANSCRIPT" 1>&2
echo "--- last 20 lines ---" 1>&2
tail -20 "$TRANSCRIPT" 1>&2
