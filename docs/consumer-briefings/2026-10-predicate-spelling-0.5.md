# Consumer briefing — 0.5 flags, part (c): one `--predicate` spelling, `[NAME:]REG == VALUE` (2026-10)

> Source of truth: [`parse_extra_predicate`](../../crates/mununu-core/src/adapter/recoverability.rs) — surface: (CLI+API)

**Audience:** rosf, monono and every script that passes `--predicate` to `btor2` / `sv verify-recoverability` or `btor2` / `sv cegar`, or sends `predicates` strings to the recoverability routes. Sixth cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.5, finding F2).

## TL;DR

- **One spelling.** `sv verify-auto --predicate` took an expression (`state_q == 3`); recoverability and cegar took `NAME:REG=VALUE`. All of them now take an expression with an optional name, `[NAME:]REG == VALUE`, and the name defaults to the expression's normalised text (`state_q == 3`). The default also removes a trap: a hand-written name used to have to match the normalised spelling for the predicate to bind.
- **The pre-0.5 spelling is refused.** `idle:state_q=3` is an error whose message gives the new spelling (`idle:state_q == 3`).
- **What each verb accepts is unchanged.** Recoverability and cegar's cube ladder takes register-equals-constant predicates. A relational or compound expression (`a == b`, `st >= 2`) on those verbs is an error that names `sv verify-auto --predicate`, which accepts them. `sv verify-auto` is unchanged.
- **API:** the `predicates` strings of `/api/v1/btor2/verify-recoverability` and `/sv/verify-recoverability` take the same spelling. The structured `PredicateSpecRequest {name, register, value}` of the cegar routes is unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter passes no `--predicate` to these verbs; check the cheatsheet's recoverability lane | rewrite any `NAME:REG=VALUE` as `NAME:REG == VALUE` |
| **monono** | any lane passing `--predicate NAME:REG=VALUE` | same rewrite |
| **mununu-ui** | none; the cegar requests are structured | none |

In this repo the tutorial, the wiki, and the `v6`, `v7`, `v7b` and caliptra `m4` validators follow.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the parser lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts pass no predicate |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run these verbs |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | spelling only |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | spelling only |

## Verification steps for every consumer

```bash
mununu btor2 verify-recoverability d.btor2 --target 'st == 0' --predicate 'st == 2' | jq -r .outcome
mununu btor2 verify-recoverability d.btor2 --target 'st == 0' --predicate 'busy:st=2'; echo $?   # 1, with the new spelling in the message
```

## Provenance

- Branch `feat/flags-predicate-0.5`. Test `parse_extra_predicate_parses_and_rejects` covers the named and unnamed forms, the pre-0.5 refusal and the relational / `>=` refusals; the recoverability suite (78 tests) is unchanged; two schemas are regenerated (field descriptions only).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- One timeout flag and positive-form API keys (part d).
