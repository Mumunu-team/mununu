# Consumer briefing — cost estimates per property, and a plan-only mode (roadmap J2, 2026-11)

> Source of truth: [`RoutingRationale::estimate`](../../crates/mununu-core/src/planner/mod.rs), [`plan_auto`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs), [`SvPlanResponse`](../../crates/mununu-core/src/api/models.rs) — surface: CLI+API (`sv verify-auto --plan-only [--json]`, `POST /api/v1/sv/plan`; the `routing.estimate` field of every `sv verify-auto` report).

> **Audience:** rosf and monono (gate runtimes, CI sizing), mununu-ui (shows rows), anyone scheduling runs.
>
> **TL;DR — additive, no verdict change.** Every row's `routing` gains `shape` (the static facts) and `estimate {time_band, memory_class, confidence, basis}`. A new plan-only mode lifts the design and returns those estimates without running any engine: `sv verify-auto --plan-only` on the CLI, `POST /api/v1/sv/plan` (same body as verify-auto) on the API. New schema `sv-plan-response.schema.json`; `sv-verify-auto-response.schema.json` regenerated.

## What changed

| item | meaning |
|---|---|
| `properties[].routing.shape` | `arith_ops`, `array_states`, `inputs`, `alternation`, `constant`, `relational_atoms` — read off the cone and formula, no solving |
| `properties[].routing.estimate.time_band` | `seconds` (< 10 s), `minutes` (< 10 min), `hour`, `wall` |
| `properties[].routing.estimate.memory_class` | `small` / `medium` / `large` — a sizing class, not bytes |
| `properties[].routing.estimate.confidence` | `high` / `medium` / `low`; for `low`, size for the band's top |
| `properties[].routing.estimate.basis` | the measured table row and its evidence |
| `sv verify-auto --plan-only [--json]` | lift + planner, no engine, exit 0; `--json` = the API document |
| `POST /api/v1/sv/plan` | same request body as `/sv/verify-auto`; returns `SvPlanResponse` (`properties[{name, label, formula, routing?}]`, `unsupported`, `lift_ms`, `bands`) |

Measured accuracy (one host, 2026-11-08/09): on the OpenTitan `edn` IP, 65 of 67 rows land in the predicted band; no row predicted `seconds` ran over 10 minutes. That score used each row's formula and recorded routing; the first end-to-end `--plan-only` run scored 42 of 67 because the multiplier cell fired on over-cap rows (the reach portfolio decides those, and a multiplier is no wall for it). The follow-up PR limits the cell to in-cap rows: 65 of 67 end to end. The named limit: in-cap exact rows with an in-cone counter take a time set by the verdict, not the shape — that cell is `minutes`, `low`.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | A CI step can call `--plan-only` first and pick a runner size or timeout from `bands`, or skip the run when every row is `seconds`. Reports gain two keys under `routing`. | Optional. If the report model is strict, accept `routing.shape` and `routing.estimate`. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | Rows can show the estimate beside the measured `wall_ms`. | Optional. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, to get the fields and the route; nothing breaks without them. | Additive. |
| rosf `docker/Dockerfile*` | No. | Consumer; additive. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

Roadmap J2. Data from the second host's runs recorded in the private e2e history (edn on `19295f9`, the differential corpus, the EXP-0031 bit-cap probes). Tests: the table cell by cell, the shape facts on a BTOR2 with a multiplier and a memory, the plan view.

## Not covered here

The private scheduler (J5) that consumes the bands; per-customer recalibration from the usage ledger; a `btor2` plan verb (the estimate is computed over the lifted model, so it can follow).

## Update (mununu#929, 2026-11-09)

The in-cone counter the estimate, the `diameter_log2` routing field, the predicted engine and the `skip-diameter-bound` note read is now any register that steps itself, by a constant or by an input, of at least 12 bits; before, it was the counter abstraction's strict down-counter detector. Values move, shapes do not: `routing.diameter_log2` can now be set where it was `null` (OpenTitan `prim_count`'s 22-bit counter, say) and `null` where an 8-bit down-counter used to set it; rows that pick it up move from `seconds` to `minutes` / `low`; a recoverability row with a counter of 12 bits or more now predicts `ranking`. Verdicts do not change: the predicted engine only orders the precision ladder, which changes which engine is credited, never the verdict. Measured on the 18-design corpus against its recorded walls: 201 of 210 in band (was 205), under-estimates 4 → 0. No Docker rebuild is needed beyond picking up the release that carries it.

