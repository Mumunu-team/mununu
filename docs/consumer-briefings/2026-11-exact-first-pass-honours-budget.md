# Consumer briefing: the per-property budget now stops the exact engine's first pass (2026-11)

> Source of truth: [`run_or_abandon`](../../crates/mununu-core/src/adapter/run_budget.rs) and [`exact_ladder_within_run_budget`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto`, with `MUNUNU_PROPERTY_BUDGET_MS` / `MUNUNU_VERIFY_BUDGET_MS` (or their defaults, 15 min and 60 min)

**Audience:** anyone who runs `sv verify-auto` under a time budget, or relies on the default ones.

## TL;DR

- **A run could ignore both budgets (mununu#1063).** The exact engine's first pass ran on the driver thread and polled the budget only between fixpoint steps. One step is a single BDD operation that cannot be interrupted; on a wide cone one such operation ran past a 60 s property budget and a 240 s run budget, and the run printed nothing.
- **The driver now stops waiting at the row's deadline** (plus 2 s). The row is `unknown` with `bottom_reason.kind = "budget-expired"` for that member, a `time-budget-exceeded` note names `exact-symbolic`, and the next member and the re-plan get the row. On the design that found this, the same command now answers in 64 s, decided by the reach rescue, where it had no answer after 15 minutes.
- **A budget stop in this pass is `unknown`, not `skipped`.** `--fail-on unknown` sees it.
- **Behaviour you may notice:** a row the exact engine used to decide *after* the budget had passed (it could not be stopped) is now left to the other members. With the default 15-minute property budget that is a row that spent more than 15 minutes inside one BDD operation.
- **No change** when no budget is in effect (`MUNUNU_PROPERTY_BUDGET_MS=0` and `MUNUNU_VERIFY_BUDGET_MS=0`): the exact engine runs on the calling thread as before.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Runs return within the budget. A row may move from `exact-symbolic` to another `decided_by`, or to `unknown` (`budget-expired`). | None, unless a gate depended on a verdict the exact engine reached past its budget: raise the budget for that gate. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: `sv verify-auto` is not a UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** an internal reproduction run, 2026-11-17 (#1063); the running process was sampled with a debugger at 20 s, 130 s and 300 s and was inside one BDD substitution each time.
- **Tests:** `work_that_never_polls_is_abandoned_at_the_deadline`, `run_or_abandon_returns_the_work_inline_or_from_the_worker_and_survives_a_panic`; the issue's command in the `mununu-sva` image on both commits of the design.

## Not covered here

- The abandoned worker thread keeps its memory until it next polls its budget or the process exits. A long run with many abandoned rows can reach the memory ceiling sooner.
- A member's notes do not survive the portfolio merge, so on a row another member then decides, the `time-budget-exceeded` note is not in the final report; the row's `decided_by` shows who decided it.
- Three other sightings on mununu#1063 (a run that does not print its report after a verdict is in the breadcrumb, and two wide designs) were not sampled and are not claimed fixed.
- Which engine runs first, and for how long, is unchanged: with the default 15-minute budget the exact engine still gets 15 minutes before the faster members are tried.
