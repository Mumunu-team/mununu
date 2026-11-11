# Consumer briefing — `--timeout-ms` now reaches the justice verb's l2s rung (2026-11)

> Source of truth: [`l2s_rung`](../../crates/mununu-core/src/adapter/justice.rs#L286) and [`l2s_undecided_note`](../../crates/mununu-core/src/adapter/justice.rs#L406) — surface: CLI+API (`btor2 verify-liveness --justice`, `btor2 verify-justice`, the justice form of `POST /api/v1/btor2/verify-liveness`).

> **Audience:** anyone running `btor2 verify-justice` / `btor2 verify-liveness --justice` or the justice form of `POST /api/v1/btor2/verify-liveness` with a budget (`--timeout-ms` / `timeout_ms`); lanes driving `scripts/hwmcc_live_study.sh`.
>
> **TL;DR — a bugfix in budget semantics, plus a more informative abstention note.** The justice budget used to bound only the exact rung; the l2s rung's reachability portfolio always ran its members at their 60 s defaults. Now the budget bounds the l2s rung too: alone (`--rung l2s`) and under `--parallel-rungs` it gets the whole budget; under the default sequential `--rung both` it gets **half** of what the 30 s exact slice leaves, and the exact remainder gets the rest. **Without a budget nothing changes.** The l2s abstention note now names the budget (or the 60 s default cap) and every portfolio member's refusal or budget abstention. No JSON shape change.

## What changed

| surface | item | before → after |
|---|---|---|
| CLI + API | `--timeout-ms` / `timeout_ms` with `--rung l2s` or `--parallel-rungs` | ignored by l2s (members at 60 s) → bounds the l2s portfolio; members' bounds follow the time left |
| CLI + API | the same under sequential `--rung both` | ignored by l2s → l2s gets half of what remains after the exact slice |
| CLI + API | `notes[]` on an l2s abstention | `l2s: the reachability portfolio left the fair-cycle monitor undecided` → `… undecided within a <ms> ms budget` or `… at the members' default budgets (60 s per subprocess member; \`--timeout-ms\` raises them)`, followed by `; <member> refused: <why>` / `; <member> abstained: <why>` |
| script | `scripts/hwmcc_live_study.sh` | passes `--timeout-ms` = `BUDGET` − 30 s (`TIMEOUT_MS` overrides); `JSON_DIR` keeps each instance's report |

**Verdict effect for budgeted callers:** a budget above 60 s can now turn l2s `unknown`s into verdicts (more time per member). A short budget (≲ 2 min under `--rung both`) gives l2s *less* than the old 60 s, so an l2s decision that needed close to 60 s may now be `unknown`. Every verdict stays sound (each member is); none can flip.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Only if you call the justice verb with a budget; you do not today. | None. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | No justice UI. | None. |
| benchmark lanes (HWMCC LIVE) | The study script now hands the lane budget to the verb; results at a budget > 300 s are not comparable with earlier rows. | Re-run rather than merge with old rows. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, to get the fix. | Engine behaviour under a budget. |
| rosf `docker/Dockerfile*` | No. | Not called with a justice budget. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

Found by the 2026-11-09/10 HWMCC'17 LIVE run at 3,600 s per instance (63 instances, `mununu-sva`, 2099556): 42 of 54 `unknown` rows stopped within 3 min with the lane budget unused; a diagnostic replay showed the exact rung abstaining on the node budget and the l2s note hiding the 60 s member cap. Reproducer: `the_l2s_rung_runs_under_the_justice_budget` (under a spent budget the parent ignores it and decides; this commit abstains and names the budget).

## Not covered here

The response-liveness verbs (`btor2 verify-liveness --request/--grant`, `sv verify-liveness`, the response form of `POST /api/v1/btor2/verify-liveness`) still run the portfolio at the 60 s member defaults with no budget flag of their own (a thread-local `run_budget`, as `sv verify-auto` installs, does bound them); `trap_certificate`'s `ClosureUndecided` drops the portfolio's reasons. Both are named here as siblings, not fixed.
