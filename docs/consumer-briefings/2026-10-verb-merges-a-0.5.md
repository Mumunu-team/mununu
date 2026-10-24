# Consumer briefing — 0.5 verb merges, part (a): `btor2 verify --engine`, `sv verify-bad` (2026-10)

> Source of truth: [`verify_safety_with_engine`](../../crates/mununu-core/src/adapter/recoverability.rs) and the `btor2 verify` / `sv verify-bad` verbs in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every script that calls `btor2 verify-safety`, `sv verify` or `POST /api/v1/sv/verify`. Ninth cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.2, finding V1).

## TL;DR

- **`btor2 verify-safety` is gone; `btor2 verify --engine cube|ic3` replaces it.** The safety verbs were `btor2 verify` (the portfolio) and `btor2 verify-safety` (the KMTS cube, with no API route). They are now one verb.
  - `--engine portfolio` is the default and is unchanged.
  - `--engine cube` is the branching-cube route that `verify-safety` ran by default.
  - `--engine ic3` is the experimental IC3ia ladder (`verify-safety --engine ic3`).
  - The single-engine routes print the `btor2 verify` JSON (`outcome`, `reachable_by` / `unreachable_by` naming the engine, `contradiction: false`, `detail` with the engine's diagnosis). They refuse `--fault`, `--target`, `--witness` and `--owned-only`.
- **The cube has an API route now.** `POST /api/v1/btor2/verify` takes `engine: "portfolio" | "cube"` (default `portfolio`); `"ic3"` stays CLI-only by the standing rule for that engine (no API until it decides real cases), and an unknown name is a 400. The response gains `detail`.
- **`sv verify` is renamed `sv verify-bad`, and `POST /api/v1/sv/verify` → `/api/v1/sv/verify-bad`.** It decides the `bad` lines yosys lowers immediate assertions to, not the SVA property set; "verify" with no suffix was easy to take for `sv verify-auto`. Behaviour and response shape are unchanged. `sv verify` and `btor2 verify-safety` are now usage errors (exit 64).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter calls `sv verify-auto` only, unaffected; check the cheatsheet for `sv verify` | `sv verify-bad` |
| **monono** | lanes that call `sv verify` or `btor2 verify-safety` | `sv verify-bad`; `btor2 verify --engine cube` |
| **mununu-ui** | any call to `POST /api/v1/sv/verify` | `/api/v1/sv/verify-bad`; optionally offer `engine: "cube"` on the btor2 verify panel |

In this repo the docs, wiki, README and API docs follow.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the verbs and routes live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts call neither verb |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run these verbs |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | cheatsheet only |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, with the lane changes | verb renames |

## Verification steps for every consumer

```bash
mununu btor2 verify d.btor2 --engine cube | jq '{outcome, reachable_by, detail}'
mununu sv verify-bad top.sv --top top | jq -r .outcome
mununu btor2 verify-safety d.btor2; echo $?          # 64
```

## Provenance

- Branch `feat/verb-merges-a-0.5`. Tests: `btor2_verify_handler_routes_the_cube_engine` (the cube decides as itself, an unknown engine is a 400), `the_0_5_verb_merges_parse_and_the_old_verbs_are_usage_errors`, the handler suites under all features, and the schema drift tests (`btor2-verify-request` / `-response` regenerated).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The liveness family merge (`verify-liveness` absorbing `-all`, `-under-fairness` and `verify-justice`): part (b).
