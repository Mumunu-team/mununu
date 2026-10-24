# Consumer briefing — 0.5 verb merges, part (b): one `verify-liveness` (2026-10)

> Source of truth: the `verify-liveness` verbs in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) and `Btor2VerifyLivenessRequest` / `SvVerifyLivenessRequest` in [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every script that calls a liveness verb or route. Tenth cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.2, finding V2).

## TL;DR

- **One liveness verb per family.** The liveness verbs and routes now map as follows; the old ones are usage errors (64) or 404s.

  | before | after (CLI) | after (API, one route) |
  |---|---|---|
  | `btor2 verify-liveness --request A --grant G` | unchanged | `/btor2/verify-liveness` `{request, grant}` (now optional fields) |
  | `btor2 verify-liveness-all --response "A => G"`… | `btor2 verify-liveness --response "A => G"`… | `/btor2/verify-liveness` `{responses: [...]}` (was `/verify-liveness-all`) |
  | `btor2 verify-liveness-under-fairness … --fairness F`… | `btor2 verify-liveness --request … --grant … --fairness F`… | `/btor2/verify-liveness` `{request, grant, fairness}` (was `/verify-liveness-under-fairness`) |
  | `btor2 verify-justice FILE [--rung --timeout-ms --parallel-rungs --json]` | `btor2 verify-liveness FILE --justice [--rung …]` | `/btor2/verify-liveness` `{justice: true, rung?, timeout_ms?, parallel_rungs?}` (was `/verify-justice`) |
  | `sv verify-liveness-all --response …` | `sv verify-liveness --response …` | `/sv/verify-liveness` `{lift, responses}` (was `/sv/verify-liveness-all`) |
  | — | **new:** `sv verify-liveness … --fairness F` | `/sv/verify-liveness` `{lift, request, grant, fairness}` |

- **The response shape is shared.** Every form answers with `Btor2VerifyLivenessResponse` (`outcome`, `property`, `decided_by`). The `justice` form adds `justice[]` (`index`, `size`, `outcome`, `decided_by`, `notes`) and `converted_from_aiger`, and reports `property: "justice (N set(s))"`. The CLI's per-form JSON (the conjunction's `responses[]`, justice's `elapsed_ms` / `rung`) is unchanged.
- **Conflicting combinations are refused.** `responses` takes no `request` / `grant` / `fairness`; `justice` takes no atoms; `--rung` / `--timeout-ms` / `--parallel-rungs` need `--justice`. The CLI exits 64, the API returns 400. The two merged request types refuse unknown keys.
- **Schemas:** `btor2-verify-liveness-request`, `-response` and `sv-verify-liveness-request` regenerated; `btor2-verify-liveness-all-request`, `btor2-verify-liveness-under-fairness-request` and `sv-verify-liveness-all-request` deleted (no route). `sv-verify-liveness-all-report` (the CLI `--response` report) stays.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the cheatsheet calls `verify-liveness` (unchanged form) | none, unless a lane uses `-all` (→ `--response`) |
| **monono** | lanes that call `verify-liveness-all`, `-under-fairness` or `verify-justice` | the flag forms above |
| **mununu-ui** | calls to `/btor2/verify-liveness-all`, `-under-fairness`, `/verify-justice`, `/sv/verify-liveness-all` | post to `/verify-liveness` with `responses` / `fairness` / `justice` |

In this repo `scripts/hwmcc_live_study.sh` (`verify-liveness --justice`), the docs, the wiki and the verify-auto note that pointed at `verify-liveness-under-fairness` follow.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the verbs and routes live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | yes, for `scripts/hwmcc_live_study.sh` | it now calls `verify-liveness --justice` |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run these verbs |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | cheatsheet only |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, with the lane changes | verb merges |

## Verification steps for every consumer

```bash
mununu btor2 verify-liveness d.btor2 --response 'req == 1 => ack == 1' --response 'a == 1 => b == 1' | jq -r .outcome
mununu btor2 verify-liveness d.btor2 --request 'req == 1' --grant 'ack == 1' --fairness 'gnt == 1' | jq -r .property
mununu btor2 verify-liveness design.aig --justice --rung exact --json | jq '.justice | length'
mununu btor2 verify-justice design.aig; echo $?        # 64
```

## Provenance

- Branch `feat/verb-merges-b-0.5`. Tests: `the_merged_liveness_route_takes_fairness_and_refuses_conflicts`, the former `-all` handler tests now on the merged route, `the_0_5_verb_merges_parse_and_the_old_verbs_are_usage_errors` (every form, every conflict, every removed verb), the `--json` contract test, and the handler suites under all features.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The remaining route renames (`/context/verify` → `/context/eval`, `/context/synthesize` → `/context/synth`, …) and schemas for the unpinned responses: the API-alignment cluster.
