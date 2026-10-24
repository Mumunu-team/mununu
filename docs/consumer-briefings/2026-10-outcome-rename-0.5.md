# Consumer briefing — 0.5 verdict envelope, part 1: `verdict` → `outcome` on every RTL verb (2026-10)

> Source of truth: [`Btor2VerifyResponse::outcome`](../../crates/mununu-core/src/api/models.rs) and the CLI reports of the same verbs — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every script that reads the JSON of the RTL property verbs. Second cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.3, approved 2026-10-23).

## TL;DR

- **The field is `outcome` everywhere.** `sv verify-auto` rows already called the verdict `outcome`; every other RTL verb called it `verdict`. They now match. The values are unchanged (`holds` / `violated` / `unknown` / `skipped`), and so are the exit codes.
- **Renamed, CLI and API alike:**
  - `btor2 verify`, `btor2 verify-safety`, `btor2 verify-liveness`, `-liveness-all`, `-liveness-under-fairness` and `verify-justice` (the top level and each `justice[]` entry);
  - `btor2 verify-recoverability`;
  - `btor2 check-fsm` and `sv check-fsm` (each `registers[]` entry);
  - `sv verify`, `sv verify-liveness`, `-liveness-all` and `sv verify-recoverability`.
  - The routes are `/api/v1/btor2/verify`, `/btor2/verify-liveness*`, `/btor2/verify-justice`, `/btor2/verify-recoverability`, `/btor2/check-fsm`, `/sv/verify`, `/sv/verify-liveness*`, `/sv/verify-recoverability` and `/sv/check-fsm`. The seven affected schemas under `docs/api-schemas/` are regenerated.
- **Not renamed in this part.** `btor2 cegar` / `sv cegar` and `codesign verify`, whose `verdict` is a cell-count object, move into the envelope with the game and synthesis verbs in the next part. Fields other than the verdict are unchanged: `decided_by` keeps its current type on each verb (a list on the portfolio verbs, a string on `sv verify-auto` rows), and so do `reachable_by` / `unreachable_by`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter reads `sv verify-auto` only (already `outcome`), unaffected; the cheatsheet reads `"verdict"` from `verify-recoverability`, `check-fsm` and `verify-liveness` | read `outcome` in `examples/cheatsheet/run.sh` |
| **monono** | any lane that reads `verdict` from these verbs | read `outcome` |
| **mununu-ui** | the typed client reads `verdict` from the btor2 / sv responses above | rename the field in `src/api/endpoints.ts` and its readers |

In this repo the readers are updated: `scripts/profile_cases.py`, `scripts/hwmcc_live_study.sh`, the `v8` / `v9` / `v10` recoverability validators, and the examples in `docs/verifying-rtl.md` and the CI wiki page.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the JSON shape lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | yes, for `scripts/profile_cases.py` | it now reads `outcome` |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not emit these reports |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | `verdict` → `outcome` on the cheatsheet verbs |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Verification steps for every consumer

```bash
mununu btor2 verify-recoverability d.btor2 --target 'st == 0' | jq -r .outcome
mununu sv check-fsm fsm.sv | jq -r '.registers[].outcome'
```

## Provenance

- Branch `feat/verdict-envelope-0.5`; the API handler tests and the schema drift tests cover the renamed fields.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- cegar, codesign, `btor2 game`, `context synth`, `/synth/gr1`, `context eval --json` and `/context/verify` join the envelope in the next part.
- The unified `Trace` type (§2.4).
