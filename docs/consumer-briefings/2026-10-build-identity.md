# Consumer briefing — 2026-10 build identity: `mununu --version` and the `sv verify-auto` report name the commit and whether the tree was clean

> **Audience:** anyone recording which mununu produced a verdict — rosf (`toolchain.lock`, its run report), monono (`MUNUNU_ENGINE` pins), and any parser of `sv verify-auto --json` / `POST /api/v1/sv/verify-auto`.
>
> **Provenance:** [mununu#715](https://github.com/Mumunu-team/mununu/issues/715), asked by rosf. Code: `crates/mununu-cli/build.rs`, `crates/mununu-core/src/build_info.rs`, `SvVerifyAutoResponse` in `crates/mununu-core/src/api/models.rs`, `docker/Dockerfile`. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

From `6f500b0` (2026-09-15) to `e7a5b21` (2026-10-17) every build printed `mununu 0.4.0`, while verdicts changed underneath it. The version stays an interface promise. The **build identity** is now a separate fact:

```
$ mununu --version
mununu 0.4.0 (e7a5b21)          # or (e7a5b21-dirty); bare `mununu 0.4.0` when the build was not told
```

and the `sv verify-auto` document carries `"build": "<full sha>"` and `"build_clean": true|false`. Both fields are omitted when the binary does not know its commit. They are never filled with a guess.

No verdict changes.

## What changed

| Surface | Before | After |
|---|---|---|
| `mununu --version` | `mununu 0.4.0` | `mununu 0.4.0 (<sha7>)`, `(<sha7>-dirty)`, or `mununu 0.4.0` when unknown |
| `sv verify-auto --json` | no build field | `build` (full sha) and `build_clean`, both optional, omitted together |
| `POST /api/v1/sv/verify-auto` | no build field | the same two fields, from the server binary |
| `docker/Dockerfile` | — | `ARG MUNUNU_BUILD_SHA` / `MUNUNU_BUILD_DIRTY` (`.git` is not in the build context) |

`build_clean: false` means a tracked file differed from the commit at build time. Untracked files are not counted.

## What to update, per consumer

- **rosf** — read `build` / `build_clean` into the run report, as planned. Keep checking `version` against `toolchain.lock` for the interface; compare `build` for "is this the engine I tested with". A missing `build` means "unknown", not "same".
- **monono** — `make engine-build` builds from a clean detached worktree, so its binaries report `(<sha7>)` with `build_clean: true`. The `--version` probe in `formal-docker` keeps working; its output now names the commit.
- **Parsers pinned to the exact JSON shape** — two new optional top-level keys. The schema is regenerated (`docs/api-schemas/sv-verify-auto-response.schema.json`).
- **Anyone building the production image** — pass the build args (see `docs/docker.md`), or the image's binary reports no identity.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | `--version` line, report fields; needs the build args to carry an identity | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; builds from the mounted repo, so git answers | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles mununu; reads the new fields | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
mununu --version
mununu sv verify-auto dut.sv --top dut --json | jq '{build, build_clean}'
cargo test -p mununu-core --lib build_info
cargo test -p mununu-cli the_version_line_carries_the_build_identity_when_known
```

## Not covered here

- The other report-producing verbs (`sv verify-recoverability`, `btor2 *`, `verify`) do not carry the fields yet. `sv verify-auto` is the document rosf and monono read. The identity is registered process-wide, so adding it to another response is one field.
- The legacy root binary (`src/`) registers no identity, so its reports omit the fields.
- A build inside a container where git refuses the mounted repository (`safe.directory`) reports no identity.
