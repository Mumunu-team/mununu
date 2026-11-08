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

## Addendum 2026-11-08 — J4c: every row is charged, and the run's wall is split

> Source of truth: the `pending_wall` charge at the top of the main loop in [`verify_auto_impl`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs); [`RunTimingView`](../../crates/mununu-core/src/api/models.rs) — surface: CLI+API.

The first real data run (edn, 67 rows, 660 s) showed the sum of `wall_ms` at 157 s with 495 s charged to no row. Two causes, one fix:

- **Rows that left the main loop early were never charged.** The single accumulation sat at the end of the loop body, after thirteen `continue` sites — the exact member's rows, the `constant` fold, the recoverability ladder, every refusal. The wall is now charged at the top of the *next* iteration (and once after the loop), so every exit path is covered. `wall_ms` now appears on `constant` rows too (it is small) and on rows the exact member refused or decided.
- **The run's own wall is a field.** `timing: {run_ms, lift_ms, properties_ms, unattributed_ms}` at the top level of the report (`unattributed_ms` = `run_ms` − `lift_ms` − Σ `wall_ms`, saturating). A large `unattributed_ms` is a finding to report, not a figure to infer from a log.

| field | type | meaning |
|---|---|---|
| `timing.run_ms` | integer | the whole run, measured at the entry of `sv verify-auto` |
| `timing.lift_ms` | integer, optional | the SV → BTOR2 lift and its re-lifts, paid once per run; absent when nothing was lifted |
| `timing.properties_ms` | integer | Σ `properties[].wall_ms` |
| `timing.unattributed_ms` | integer | what no row and not the lift accounts for |

Consumers: additive; a strict report model adds the optional `timing` object. Expect `properties_ms` to rise on the same design versus a report before this change — rows that were silently free are now charged, verdicts are unchanged.
