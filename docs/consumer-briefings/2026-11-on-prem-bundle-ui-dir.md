# Consumer briefing — the on-prem bundle and `mununu server --ui-dir` (roadmap H5, 2026-11)

> Source of truth: [`create_router_with_ui`](../../crates/mununu-core/src/api/server.rs), [`docker/Dockerfile.bundle`](../../docker/Dockerfile.bundle), [`scripts/bundle.sh`](../../scripts/bundle.sh) — surface: CLI+API (`mununu server --ui-dir DIR`; the API routes unchanged, served beside the UI).

> **Audience:** mununu-ui (its build is what the bundle serves), rosf and monono (an image they may deploy instead of their own), anyone running `mununu server`.
>
> **TL;DR — additive.** `mununu server` gains `--ui-dir DIR`: it serves the static UI from `DIR` at `/` and the API at `/api/v1/…` on the same port and origin. A new image, built by `scripts/bundle.sh` from `docker/Dockerfile.bundle`, carries the binary, one pinned oss-cad-suite release and the UI, with a compose file, a `mununu.toml` and a licence-file mount. No route, verdict or JSON shape changes. Filed after the fact by review session R0 (#912 merged without one).

## What changed

| item | meaning |
|---|---|
| `mununu server --ui-dir DIR` | serve `DIR` (a UI build) at `/`; any path not matched by a route or a file falls back to `DIR/index.html` |
| the bundle image | `mununu` + oss-cad-suite (one tag, the one `mununu-sva` pins) + the UI at `/srv/mununu-ui`; entrypoint `mununu server --addr 0.0.0.0:8080 --config /etc/mununu/mununu.toml --ui-dir /srv/mununu-ui` |
| `deploy/compose.yaml` | maps the port to `127.0.0.1:8080` on the host, mounts `mununu.toml`, `mununu.lic` and `./work` |
| notices (since #932) | `/usr/share/doc/mununu/`, `/opt/oss-cad-suite/license/`, `/srv/mununu-ui/LICENSE.txt` and `THIRD_PARTY_NOTICES.txt`; `deploy/NOTICE.md` lists them |

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **mununu-ui** | The bundle serves your `npm run build` output, built with `VITE_API_URL=/api/v1` so the API is same-origin (no CORS, no second port). Since #932 the bundle refuses a build without `dist/LICENSE.txt` and `dist/THIRD_PARTY_NOTICES.txt`, which `npm run build` writes since mununu-ui#90. | None, unless a build sets `VITE_API_URL` to another origin. |
| **rosf** | You can deploy the bundle image instead of building mununu yourself; the API is the same. The API has no authentication (#934): keep the compose loopback mapping or put an authenticating proxy in front. | Optional. |
| **monono** | Same as rosf. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| the on-prem bundle (`scripts/bundle.sh`) | Yes, from a commit at or after #932 (#912's Dockerfile copied the removed `src/` once #925 landed). | Build fix and notices. |
| `mununu-sva` / the release archives | No. | `--ui-dir` is only needed to serve a UI. |
| rosf `docker/Dockerfile*` | No. | Consumer. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Consumer. |

## Provenance

Roadmap H5 (#912, `74368a97`); build fix and notices #932 (`b0e8a0b0`); air-gapped install check recorded with #912. Review session R0 found the missing briefing.

## Not covered here

Authentication and the CORS policy (#934), confining request-body paths to `/work` (#935), traversal tests for `--ui-dir` (#936), the `/api` 404 fallback (#940), checksum-pinning the suite download (#937), and Track P's `mununu server --ui` with embedded assets.
