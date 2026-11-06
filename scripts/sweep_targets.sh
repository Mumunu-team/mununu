#!/usr/bin/env bash
# Reclaim Cargo build artifacts in mununu's siblings under ~/git_repo/.
#
# Walks every immediate sibling directory of the repo this script lives in
# (i.e. peers of mununu under ~/git_repo/), and for any sibling whose root
# carries a Cargo.toml runs `cargo sweep --time N` on it. cargo-sweep deletes
# build outputs whose access time is older than N days, keeping the most
# recent build cache warm so day-to-day iteration is unaffected.
#
# Default age threshold: 14 days. Override with SWEEP_DAYS=N.
# Defaults to dry-run; set SWEEP_APPLY=1 to actually delete.
#
# Skips Docker volumes/images entirely — cleaning those is a separate concern.
#
# Install cargo-sweep once: cargo install cargo-sweep
# Manual run:                make sweep              (this script via Makefile)
# Schedule weekly:           a launchd unit that runs this script with SWEEP_APPLY=1. It is
#                            machine-local (absolute paths), so it is kept with the private ops
#                            files (mununu-private/scripts/launchd/), not in this repo.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SIBLINGS_ROOT="$(cd "$REPO_ROOT/.." && pwd)"
DAYS="${SWEEP_DAYS:-14}"
APPLY="${SWEEP_APPLY:-0}"

if ! command -v cargo-sweep >/dev/null 2>&1; then
    echo "cargo-sweep not installed. Install with: cargo install cargo-sweep" >&2
    exit 1
fi

if [ "$APPLY" = "1" ]; then
    DRY_FLAG=""
    mode="apply"
else
    DRY_FLAG="--dry-run"
    mode="dry-run (set SWEEP_APPLY=1 to delete)"
fi

echo "Sweeping Cargo targets older than ${DAYS}d under ${SIBLINGS_ROOT} — ${mode}"
echo

reclaimed_kb=0

for d in "$SIBLINGS_ROOT"/*/; do
    [ -f "${d}Cargo.toml" ] || continue
    name="$(basename "$d")"
    target="${d}target"

    before_kb=0
    if [ -d "$target" ]; then
        before_kb=$(du -sk "$target" 2>/dev/null | awk '{print $1}')
    fi

    printf "==> %s\n" "$name"
    cargo sweep --time "$DAYS" $DRY_FLAG "$d" 2>&1 | sed 's/^/    /'

    if [ "$APPLY" = "1" ] && [ -d "$target" ]; then
        after_kb=$(du -sk "$target" 2>/dev/null | awk '{print $1}')
        diff=$(( before_kb - after_kb ))
        if [ "$diff" -gt 0 ]; then
            reclaimed_kb=$(( reclaimed_kb + diff ))
            printf "    reclaimed %s MiB\n" "$(awk "BEGIN { printf \"%.1f\", $diff/1024 }")"
        fi
    fi
done

if [ "$APPLY" = "1" ]; then
    printf "\nTotal reclaimed: %s MiB\n" "$(awk "BEGIN { printf \"%.1f\", $reclaimed_kb/1024 }")"
fi
