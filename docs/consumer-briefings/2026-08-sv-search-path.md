# Consumer briefing — 2026-08 `sv lint` / verify verbs `--search-path <DIR>` (mununu#475 item 3)

> **Audience:** primarily **monono** (the adoption reporter for the cross-directory submodule gap), secondarily **ROSF** (no direct impact; touches CLI-only surface).
>
> **Fix:** mununu#475 item 3, closing the fifth of five ergonomics gaps in the original report. The prior four items shipped in the previous briefing at [`2026-08-sv-lint-mutate-ergonomics.md`](2026-08-sv-lint-mutate-ergonomics.md).
>
> **User-facing docs:** [`docs/verifying-rtl.md`](../verifying-rtl.md) §`sv lint` — the new "Recent additions" callout on `--search-path`.
>
> **TL;DR:** one new CLI flag `--search-path <DIR>` on the shared SV-lift args (so every `sv` verb inherits it) unblocks the last piece of monono's whole-tree workflow — single-file lifts with cross-directory submodule instantiations. No verdict semantics change; the plumbing is additive-only.

## What changed

- **`--search-path <DIR>`** on `SvLiftArgs` — inherited by `sv verify`, `sv verify-auto`, `sv verify-liveness`, `sv verify-liveness-all`, `sv verify-recoverability`, `sv check-fsm`, `sv cegar`, `sv lint`, `sv mutate`, `sv extract-sva`. Recursively scans DIR for `.v` / `.sv` files (using `discover_sv_files`, same walker `--design-dir` uses) and stages them alongside the primary input. Honours `--exclude` — the same directory-component skip list applies to the scan.
- **Dedup discipline** — the scanner suppresses two collision classes: canonical-path collisions (the same file listed via both `--source` and `--search-path`) and short-name collisions (a search-path file with the same filename as the primary or a staged source — yosys would error on duplicate module names). Silently additive.
- **Composable with `--design-dir`** — a primary block from `--design-dir` plus sibling libraries from one or more `--search-path <DIR>` invocations, for a design + peer-utility layout.

## What did NOT change

- `PropertyVerdict` — unchanged.
- `AutoVerifyReport` — unchanged.
- Existing CLI flags — unchanged shape and semantics.
- HTTP API routes — unchanged. `--search-path` is CLI-only (like `--design-dir` / `--include-dir` / `--exclude` — an on-disk directory scan has no analog on the API/UI, whose flat name-staging already lets the caller stage whichever sources they want).
- Engine behaviour on any previously-decided property — unchanged.

## For monono-agents (direct CLI consumer)

**What to update on your side:** at minimum, nothing — old commands still work. To adopt the new flag on the previously-blocked 53/109 files:

- Replace hand-listed `--source` chains with `mununu sv lint <primary.sv> --search-path <peer_lib_dir>`. `--search-path` is repeatable if peers live in more than one directory.
- Combine with `--exclude` if your tree has directories the search-path scan should skip: `mununu sv lint <primary.sv> --search-path <peers> --exclude faulty`.
- If you were previously working around the gap by pointing `--design-dir` at a parent directory, that still works and remains the right choice when you want the whole tree treated as one design; `--search-path` is the answer when you want ONE primary file with a set of module-dependency directories staged.

**What to expect:** the reporter's `mem_sched.sv` → `slot_arbiter` case should now lift cleanly under `sv lint mem_sched.sv --search-path <lib_dir>`. The 53/109-unliftable count in the reporter's tree should drop toward zero for any file whose only lift blocker was cross-directory module resolution.

**Docker rebuild:** monono's Docker (if any) needs rebuild only if it pins a specific mununu binary version. No sidecar or config change required.

## For ROSF-agents (subprocess `--profile industrial`)

**What to update on your side:** nothing required. This PR doesn't touch the `--profile industrial` verify-auto engine path, and `--search-path` is CLI-only (the API stages sources by name, so directory-scan flags have no analog there).

**What to expect:** if ROSF ever shells out to the mununu CLI directly (as opposed to via the industrial-profile API path) and hits a cross-directory submodule case, `--search-path` is available.

**Docker rebuild:** `rosf/docker/Dockerfile` needs rebuild only if it pins a specific mununu binary version.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod CLI) | binary carries one new CLI flag `--search-path` | Yes if consumers pin a version tag |
| mununu `Dockerfile.dev` | binary bump | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; no e2e-test behaviour change | Only if a downstream requires the new binary |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `docker/Dockerfile` | consumes mununu subprocess; no engine-behaviour change in this PR | Only if pinned to a version tag; behavior updates on next binary pull otherwise |
| rosf `docker/Dockerfile.dev`, `.hw` | no direct dependence on the changed paths | No |
| monono Docker (any) | consumes `sv lint` and `sv verify-auto` CLI | Only if pinned to a version tag |

## Shared footer — verification you should run after adopting

1. **Confirm the cross-directory unblock**: run `mununu sv lint <primary.sv> --search-path <peer_dir>` on a case that previously errored with `unknown module`. It should lift cleanly.
2. **Confirm `--exclude` composition**: `mununu sv lint <primary.sv> --search-path <peer_dir> --exclude <bad_subdir>` should skip the excluded subtree just as `--design-dir --exclude` does.
3. **Confirm the composability with `--design-dir`**: `mununu sv verify-auto --design-dir <block> --search-path <peer_libs>` should stage BOTH the block's own files AND the peer-lib scans, deduped.

## Provenance

- Fix commit: (pending merge — see branch `fix/475-search-path-single-file` in the mununu repo)
- Issue: mununu#475 item 3 — <https://github.com/Mumunu-team/mununu/issues/475>
- Policy this briefing was written to satisfy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md)
- Sibling briefing (items 1, 2, 4, 5): [`2026-08-sv-lint-mutate-ergonomics.md`](2026-08-sv-lint-mutate-ergonomics.md)

## Not covered here (follow-ups)

- **API/UI parity** for `--search-path`. The design deliberately keeps this CLI-only (matches `--design-dir`, `--include-dir`, `--exclude` — an on-disk directory scan has no meaningful API analog).
- **`--filelist <PATH>`** — the alternative to `--search-path`, accepting a filelist.f-style file listing sources. Not needed for the reporter's case; a future add if a filelist-driven workflow emerges.
