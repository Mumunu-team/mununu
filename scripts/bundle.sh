#!/usr/bin/env bash
# Roadmap H5 — build the on-prem bundle: the UI's static build → docker/Dockerfile.bundle → a
# `docker save` tarball + sha256, beside deploy/compose.yaml, deploy/mununu.toml and deploy/README.md,
# under dist-bundle/<version>/. deploy/README.md documents the install; docs/install.md § T1 points at it.
#
#   scripts/bundle.sh                       # version from Cargo.toml, UI from ../mununu-ui
#   MUNUNU_UI_DIR=/path/to/mununu-ui scripts/bundle.sh
#   SKIP_UI=1 scripts/bundle.sh             # reuse deploy/ui-dist from a previous run
set -euo pipefail
cd "$(dirname "$0")/.."
ver=$(sed -nE 's/^version = "([^"]+)"/\1/p' crates/mununu-cli/Cargo.toml | head -1)
sha=$(git rev-parse HEAD); dirty=0; git diff --quiet HEAD -- . ':!deploy/ui-dist' 2>/dev/null || dirty=1
ui="${MUNUNU_UI_DIR:-../mununu-ui}"
out="dist-bundle/$ver"
echo "== mununu bundle $ver ($sha${dirty:+, dirty=$dirty})"

if [ "${SKIP_UI:-0}" != 1 ]; then
  [ -f "$ui/package.json" ] || { echo "bundle.sh: no mununu-ui at $ui (set MUNUNU_UI_DIR)" >&2; exit 2; }
  echo "== UI build ($ui), same-origin API"
  ( cd "$ui" && { [ -d node_modules ] || npm ci --no-audit --no-fund; } && VITE_API_URL=/api/v1 npm run build >/dev/null )
  rm -rf deploy/ui-dist && cp -R "$ui/dist" deploy/ui-dist
fi
[ -f deploy/ui-dist/index.html ] || { echo "bundle.sh: deploy/ui-dist/index.html missing" >&2; exit 2; }
echo "== image"
docker build -f docker/Dockerfile.bundle -t "mununu-bundle:$ver" \
  --build-arg MUNUNU_BUILD_SHA="$sha" --build-arg MUNUNU_BUILD_DIRTY="$dirty" .
echo "== tarball"
mkdir -p "$out"
docker save "mununu-bundle:$ver" | gzip > "$out/mununu-bundle-$ver.tar.gz"
( cd "$out" && shasum -a 256 "mununu-bundle-$ver.tar.gz" > "mununu-bundle-$ver.tar.gz.sha256" )
cp deploy/compose.yaml deploy/mununu.toml deploy/README.md "$out/"
echo "== done"
ls -l "$out"
echo "install: see $out/README.md (docker load, the three files, docker compose up -d)"
