# Consumer briefing — 0.5 verdict envelope, part 2: games, synthesis, CTXDSL verification, `context eval --json` (2026-10)

> Source of truth: [`game_outcome` / `realizable_outcome` / `context_outcome`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every script that reads the JSON of the game, synthesis, CTXDSL verification, codesign or cegar verbs. Third cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.3; part 1 is [`2026-10-outcome-rename-0.5.md`](2026-10-outcome-rename-0.5.md)).

## TL;DR

- **`btor2 game` gets `outcome` and `detail`** on the CLI and on `/api/v1/btor2/game`. Without `--fault`, `outcome` is the controller's objective: `holds` iff `realizable`. With `--fault`, the controller is the attacker and the property is the design's resistance, so `realizable` (an attack wins) is **`violated`**. `detail` says this in words, so the polarity is no longer visible only through the exit code. `realizable` and `attack_exists` stay. The exit code reads `outcome`, as before.
- **Synthesis reports `outcome` beside `realizable`** (`holds` iff a controller exists) on `POST /api/v1/context/synthesize`, `POST /api/v1/synth/gr1` and the `context synth --dump-json` document.
- **`/api/v1/context/verify` and `/api/v1/codesign/verify`: `satisfied` and `all_satisfied` are replaced by `outcome`.** Per result, `outcome` is `holds` iff every initial state satisfies the formula. At the top level it is the worst result, and `unknown` when every formula holds but a `sanity` report found something (the same rule as `context eval`'s exit code). The `codesign verify --json` CLI document gets a top-level `outcome`, and its `verdict` object is renamed `property` (without `satisfied`).
- **`mununu verify --json` (`verify.toml` projects): `property_verdicts[].satisfied` is replaced by `outcome`** (`holds` / `violated`). Read it together with `fair_path_exists` under assumptions, as before.
- **`context eval --json`** prints the `/context/verify` document (`success`, `outcome`, `results[]`, `sanity[]`) instead of the text, and a JSON error document on failure. Exit codes as in the 0.5 exit table.
- **cegar: `verdict` is renamed `cells`** (`btor2 cegar` / `sv cegar`, the CLI and `/api/v1/btor2/cegar`, `/sv/cegar`, including `iterations[].cells`). The counts are over every cell, not only the initial ones, so cegar carries no single `outcome`; the rename says what the object is.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter reads `sv verify-auto` only, unaffected; the cheatsheet's `btor2 game` lane can read `outcome` / `detail` | optional |
| **monono** | `verify --json` `property_verdicts[].satisfied` → `outcome`; `context eval` prose greps can move to `--json` | read `outcome == "holds"`; replace the `Initial states satisfying: 1/1` greps with `context eval --json \| jq -r .outcome` |
| **mununu-ui** | `/context/verify` `all_satisfied` / `results[].satisfied`, `/codesign/verify` `satisfied`, cegar `verdict` → `cells` | update `src/api/endpoints.ts` and their readers; `outcome` is new on game and synthesis responses |

In this repo, the 20 `examples/verify/*/validate.sh` readers of `property_verdicts[].satisfied` and the caliptra UI end-to-end script (cegar `cells`) are updated, along with `wiki/API-Reference.md` and `docs/verifying-rtl.md`.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the JSON shapes live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts read cegar cell counts by key, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not emit these reports |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | additive for the adapter |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | `verify --json` field rename |

## Verification steps for every consumer

```bash
mununu context eval examples/wiki/door_lock.ctxdsl --automaton DoorLock --formula lockout_reachable --json | jq -r .outcome   # holds
mununu verify verify.toml --json | jq -r '.property_verdicts[] | "\(.name) \(.outcome)"'
mununu btor2 game d.btor2 --good 'done == 1' --controllable c --fault st:1 | jq '{outcome, detail, realizable}'
```

## Provenance

- Branch `feat/verdict-envelope-0.5-part2`. Tests: `a_game_under_faults_reads_realizable_as_an_attack_that_violates_resistance`; the handler suites under all features; the verify orchestrator suite (135 tests, read through `PropertyVerdict::satisfied()`); `context eval --json` run by hand on `examples/wiki/door_lock.ctxdsl` (a formula, `--sanity` alone, an unknown formula → the JSON error document, exit 1).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Route renames (`/context/verify` → `/context/eval`, `/context/synthesize` → `/context/synth`, `/synth/gr1` folded into synth, …) are the API-alignment cluster.
- The unified `Trace` type (§2.4), and schemas for the game, synthesis and CTXDSL responses.
