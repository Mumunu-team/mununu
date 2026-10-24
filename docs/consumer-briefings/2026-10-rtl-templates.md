# Consumer briefing — RTL property templates on `sv verify-auto` (2026-10)

> Source of truth: [`template_guarantees`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + the `rtl_*` entries of [`builtin_templates.json`](../../crates/mununu-core/src/adapter/templates/builtin_templates.json) — surface: (CLI+API) — `sv verify-auto --template ID --template-arg [ID.]KEY=VALUE [--templates-file FILE]`, `POST /api/v1/sv/verify-auto` `templates: [{template, args}]`

**Audience:** rosf, monono, mununu-ui, and anyone who verifies RTL that carries few assertions.

## TL;DR

- **New, opt-in:** `sv verify-auto` checks property templates beside the design's own SVA. Seven `rtl_*` templates take RTL conditions (`rtl_atom` parameters): `rtl_never`, `rtl_invariant`, `rtl_mutex`, `rtl_hold_until`, `rtl_response` (`AG(T → AF R)`, inevitability), `rtl_recoverable` (`AG EF T`), `rtl_reachable` (`EF T`). Each instance is a row labelled `tmpl.<instance>`.
- **Enum names resolve** in `@mununu_guarantee` and template atoms: `state_q == SCRAM` is checked against the enum member's value (equality and inequality; `TranslationReport::named_constants`). Before, an annotation had to spell the encoding.
- **Human output:** a property with a label now prints an extra `label: …` line under its `[kind] name: VERDICT` line. The `[kind] name: VERDICT` line itself is unchanged.
- **New request field:** `templates` on `SvVerifyAutoRequest` (schema regenerated). No other shape change; existing runs without templates are byte-identical apart from the `label:` line for already-labelled properties.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the transcript gains a `label:` line under labelled properties (`@mununu_guarantee(name)`, labelled SVA) | if a script parses the lines after `[assert]`, skip `label:` like `formula:` |
| **monono** | same | same |
| **mununu-ui** | optional `templates` request field | none required (RTL verb is CLI+API) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, to use templates | new flags |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | verify-auto runs here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | the `label:` line |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | the `label:` line |

## Measured

| run | result |
|---|---|
| enum FSM (`IDLE/WITHDRAW/HOLD/SCRAM/FAULT`, `FAULT` absorbing) in mununu-sva, four templates | `rtl_never(state_q == 5)` HOLDS, `rtl_response(trip && state_q != FAULT → state_q == SCRAM)` HOLDS, `rtl_recoverable(state_q == IDLE)` VIOLATED with a stall lasso ending in `FAULT`, `rtl_reachable(state_q == FAULT)` HOLDS; the enum names resolved to 4 / 3 / 0 |
| `a_template_becomes_a_named_guarantee`, `named_constants_resolve_on_the_right_of_equalities_only`, `template_args_group_by_their_template` | pass (a non-boolean RTL argument is refused; dotted names, ordering comparisons and registers are left alone) |

## Provenance

- Commit: pending merge — branch `feat/rtl-templates` (on the template-pack layering, #768).
- Docs: [`wiki/Property-Templates.md`](../../wiki/Property-Templates.md) §"On RTL", [`docs/verifying-rtl.md`](../verifying-rtl.md) §2.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Bounded response as a template (SVA `a |-> ##[1:K] b` covers it).
- Templates on the other SV verbs (`sv verify-recoverability` already takes `--target`).
