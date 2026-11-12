#!/usr/bin/env bash
# mununu#991 — does a mununu server image reap orphaned processes?
#
# A timed-out subprocess member's process group is killed; its forked solvers are re-parented to
# PID 1. With `mununu` itself as PID 1 nobody `wait`s for them and each stays a zombie for the
# container's lifetime. The images run `tini` as PID 1 for that. This starts the image's default
# command (the server), leaves one orphan behind with `docker exec`, and counts zombies after it
# exits: 0 passes, anything else fails.
#
#   scripts/check-container-reaps.sh mununu:latest
#   scripts/check-container-reaps.sh mununu-bundle:0.5.12
#
# Falsifier, measured 2026-11-12 on debian:bookworm-slim: the same probe under a non-reaping PID 1
# leaves 1 zombie; under tini, 0.
set -euo pipefail
image="${1:?usage: $0 <image>}"
c=$(docker run -d --rm "$image")
trap 'docker rm -f "$c" >/dev/null 2>&1 || true' EXIT
sleep 3
if [ "$(docker inspect -f '{{.State.Running}}' "$c")" != true ]; then
  echo "FAIL: the container did not stay up"; docker logs "$c" | tail -20; exit 1
fi
pid1=$(docker exec "$c" cat /proc/1/comm)
docker exec "$c" sh -c 'sleep 1 & exit 0'
sleep 3
zombies=$(docker exec "$c" sh -c 'for s in /proc/[0-9]*/stat; do cut -d" " -f3 "$s"; done 2>/dev/null' | grep -c '^Z$' || true)
echo "image=$image pid1=$pid1 zombies=$zombies"
[ "$zombies" = 0 ] || { echo "FAIL: PID 1 does not reap orphans"; exit 1; }
echo PASS
