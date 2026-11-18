# Consumer briefing: a `GF` fairness assumption now reaches every row it binds (2026-11)

> Source of truth: [`reopen_violations_decided_without_fairness`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) and [`BottomReason::ViolationWithoutFairness`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` on a source with `// @mununu_assume GF <REG op VALUE>`

**Audience:** anyone who writes `@mununu_assume GF …` and reads the verdicts of response properties (`a |-> s_eventually b`, `AG(a → AF b)`).

## TL;DR

- **A false `violated` is removed (mununu#1073).** The fairness assumption was applied only to rows the first-pass engines left `unknown`. A row the exact engine or the cube decided was decided without it, so the same file could read `violated` with the default engine and `holds` with `--engine explicit`, both beside a note saying the assumption was "auto-applied".
- **A response row refuted by a first-pass member is now decided again under the assumption.** The verdict that ships is the fair-cycle route's, with a `fair-cycle-rescue` note naming the assumption.
- **New `bottom_reason.kind`: `violation-without-fairness`.** When the fair-cycle route cannot decide the row (a fairness atom that binds no register, or no verdict in the rescue slice) the row is `unknown` with this reason, never the unfair `violated`.
- **Note text:** the `annotation-properties` item for a `GF` assume no longer says "auto-applied"; it says what the assumption binds. What happened to a row is in that row's note or reason.
- **No change** for a run with no `GF` assumption, for rows that are not response-shaped, and for any `holds`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A response row that was `violated` under a `GF` assume may become `holds` or `unknown` (`violation-without-fairness`). One new `bottom_reason.kind` value. | Add the kind to any closed list of reasons; re-run gates that recorded such a row. |
| **monono** | Same. `@mununu_template` rows with a response shape are covered by the same rule. | Same. |
| **mununu-ui** | None: `sv verify-auto` is not a UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** an internal reproduction run, 2026-11-17 (#1073), on two designs independently; reproduced on a 14-line design.
- **Tests:** `a_response_row_refuted_without_its_fairness_is_decided_again_under_it`, `a_refutation_the_fair_route_cannot_check_is_withheld_with_its_reason`, `fairness_reopens_only_refuted_response_rows`.
- **Schema:** `docs/api-schemas/sv-verify-auto-response.schema.json` (the reason table).

## Not covered here

- Fairness atoms are still single register comparisons. An atom over a wire or a primary input does not bind in the fair-cycle monitor; such a row is now `unknown` with the reason, where it used to be `violated`.
- The exact engine does not decide fair liveness itself; the fair-cycle route does.
- `sv verify-liveness --fairness` and `btor2 verify-liveness --fairness` are unchanged.
