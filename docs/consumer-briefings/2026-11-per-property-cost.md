# Consumer briefing — `wall_ms` and `routing` on every property (roadmap J4, 2026-11)

> Source of truth: [`PropertyVerdictView`](../../crates/mununu-core/src/api/models.rs) (`wall_ms`, `routing`), filled from [`AutoVerifyReport::property_wall_ms` / `routing`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API (`sv verify-auto --json` and `POST /api/v1/sv/verify-auto`).

> **Audience:** rosf and monono (parse the report), mununu-ui (shows rows), anyone sizing runs.
>
> **TL;DR — two additive fields per property, no verdict change, no behaviour change.** Every row of the `sv verify-auto` report now carries `wall_ms` (the measured wall of that property: its main pass plus any ⊥ re-plan arm) and, when the planner routed it, `routing: {class, cone_bits, cap, diameter_log2, predicted_engine}` — the same facts the `plan-cost` note has rendered as prose since mununu#548, now as data. The stderr `[mununu-timing]` line (`MUNUNU_PROPERTY_TIMING=1`) prints the same number and stays opt-in. `sv-verify-auto-response.schema.json` is regenerated.

## What changed

| field | type | meaning |
|---|---|---|
| `properties[].wall_ms` | integer, optional | milliseconds this property took: the main pass plus any ⊥ re-plan arm, summed. Absent when the row was never timed (a row the run never reached). It is the property's own time; the design's lift is paid once per run and is **not** in it. |
| `properties[].routing` | object, optional | `class` (`Safety` / `Reachability` / `Liveness` / …), `cone_bits`, `cap` (the exact engine's effective bit cap), `diameter_log2` (the widest in-cone down-counter, or null), `predicted_engine` — the planner's static prediction, telemetry, never a verdict. Absent on the single-engine routes that compute no rationale. |

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Two more keys per property; nothing moved. A gate that times its own rows can now read the tool's number instead. | None required. If the report model is strict, add the two optional fields. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | The rows can show their cost and predicted engine. | Optional. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images | Yes, to get the fields; nothing breaks without them. | A report-shape addition. |
| rosf `docker/Dockerfile*` | No. | Consumer of the report; additive fields. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

- Roadmap J4 (the estimator's calibration), pulled ahead of J2 because J2's falsifier — "do static shape features place ≥ 80 % of rows in the right time band?" — needs a measured wall and the features per row, and the recorded corpus runs had the features only (the timing line was never enabled). `mununu-private/tools/j2_band_check.py` reads both.
- The lift's per-run cost is deliberately not folded into `wall_ms`; if the first measurement shows the per-property sum does not account for the run, the lift's cost gets its own report field.

## Not covered here

- The estimator itself (`--plan-only`, `POST /api/v1/sv/plan`, `estimate: {time_band, memory_class, confidence}`) — J2, after its falsifier reads.
