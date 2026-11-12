# Consumer briefing: the server images run `tini` as PID 1, and the production image builds again (2026-11)

> Source of truth: [`docker/Dockerfile`](../../docker/Dockerfile), [`docker/Dockerfile.bundle`](../../docker/Dockerfile.bundle), [`scripts/check-container-reaps.sh`](../../scripts/check-container-reaps.sh) — surface: CLI+API (the images serve both)

**Audience:** anyone who builds or runs the production image (`docker/Dockerfile`) or the on-prem bundle (`docker/Dockerfile.bundle`), with `docker run` or `deploy/compose.yaml`.

## TL;DR

- **The defect (#991).** Both images ran `mununu` as PID 1. When a subprocess member (rIC3, btormc, Pono, …) times out, its process group is killed and its forked solvers are re-parented to PID 1. `mununu` never `wait`s for them, so each one stayed a zombie for the server's lifetime and held a pid. rIC3's portfolio mode forks about 11 per call, so a busy server could fill the pid table.
- **Now:** the entrypoint is `/usr/bin/tini -- mununu`. tini reaps every orphan. Arguments still go to `mununu`, so `docker run <image> sv verify-auto …` works as before.
- **Measured on the rebuilt production image:**
  - `scripts/check-container-reaps.sh` leaves one orphan in the running server and finds 0 zombies, with PID 1 = `tini`.
  - The same image started with `--entrypoint mununu` keeps 1 zombie.
- **The production image did not build before this change.** It copied the `src/` directory removed in #925, its builder had no libz3 to link against, and its healthcheck called a `curl` the image did not install. Its builder now matches the bundle's: Rust 1.95 on trixie with libz3 4.13.3, on a trixie runtime with `libz3-4`, `curl` and `tini`. It builds, `mununu --version` runs, and the health endpoint answers through `curl`.
- **`--entrypoint mununu`** (a one-shot CLI run) bypasses tini. That is fine: the container, and its zombies, go when the run ends. For a long-running server keep the default entrypoint; `docker run --init` also works.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A server image you run no longer accumulates zombie solvers. | Rebuild or re-pull, then bump the image pin. |
| **monono** | Same, if it runs a mununu server image. | Same. |
| **mununu-ui** | None. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Yes. | The new entrypoint and builder. It did not build at all before. |
| the on-prem bundle (`docker/Dockerfile.bundle`) | Yes. | The new entrypoint. |
| `mununu/docker/Dockerfile.sva`, `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | Their entrypoints are unchanged. The e2e suite runs `make` in them, not a long-lived server. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a mununu server image: bump the pin. | They consume the image. |

## Provenance

- **Origin:** mununu#991, found while fixing #981. The tests `member_kill_875` and `probe_timeout_876` failed under `docker run` and passed under `docker run --init`.
- **Check:** `scripts/check-container-reaps.sh <image>`. Its falsifier was measured on a stand-in image (`debian:bookworm-slim`): 1 zombie with a non-reaping PID 1, 0 under tini. It was then run on the rebuilt production image: PASS with tini, and 1 zombie under `--entrypoint mununu`.

## Not covered here

- **The bundle image was not rebuilt here,** because the build needs the UI dist and the oss-cad-suite download. Its change is the same `tini` install and entrypoint. Run the check on it when it is next built (`scripts/bundle.sh`).
- **No CI job builds the production image,** which is how the broken build went unnoticed.
