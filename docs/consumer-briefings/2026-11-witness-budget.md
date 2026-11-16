# Consumer briefing: `--witness` runs inside `--timeout-ms` and says why a trace is missing (2026-11)

> Source of truth: [`witness_for_violation`](../../crates/mununu-core/src/adapter/btor2/witness.rs) — surface: CLI+API — `mununu btor2 verify --witness` and `POST /api/v1/btor2/verify` with `witness: true`

**Audience:** anyone who calls `btor2 verify` with `--witness` / `witness: true`, or who reads `trace_unavailable_within_k`.

## TL;DR

- **`--timeout-ms` now bounds the whole call (mununu#1045).** Before, it bounded only the portfolio. The trace re-derivation that follows a `violated` verdict (rIC3 under the rescue slice, then native BMC up to `--witness-max-k`) had no clock. A call asked for 300 s ran past 1,500 s and, killed from outside, wrote no JSON. The re-derivation now gets what the verdict left: rIC3's slice is clamped to the time left, and native BMC stops at the deadline.
- **New field `trace_unavailable_reason`** (string; set only when the verdict is `violated` and no trace shipped, otherwise `null` in the CLI JSON and omitted from the API response). It names the cause: the bound was searched without a path, the time budget ran out at depth *k*, the budget was spent by the verdict, the design could not be encoded, or the solver returned no model.
- **`trace_unavailable_within_k` is narrower.** Before, it was set to `--witness-max-k` whenever no trace shipped, whatever the cause. Now it is set **only** when native BMC searched every frame up to the bound and found no path. On a budget stop or an error it is unset (`null` in the CLI JSON, omitted from the API response), and the reason says what happened.
- **API: new request field `timeout_ms`** (the CLI's `--timeout-ms`). It bounds the portfolio, using the same parallel driver as the CLI, and the re-derivation. Absent keeps the previous behaviour.
- **No verdict changes.** `outcome`, `reachable_by` and the other member lists are unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | If you read `trace_unavailable_within_k` as "no trace", it is now unset on a budget stop. | Test `trace == null` for "no trace" and read `trace_unavailable_reason` for why. |
| **monono** | same; and a `--witness` call now returns inside `--timeout-ms` instead of running on | same; drop any outer timeout you added for the witness phase, or keep it as a backstop |
| **mununu-ui** | no UI surface for `btor2 verify` | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the bounded witness phase and the new field. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image. | They consume the binary. |

## Measured

- `circular_pointer_top_w16_d64_e0` (HWMCC'20 bv), `--timeout-ms 300000 --witness --witness-max-k 200`, `mununu-sva`: the call returns at 300.0 s with `violated` (`ric3-bmc`), `trace: null`, `trace_unavailable_within_k: null` and `trace_unavailable_reason: "the witness search stopped: native BMC: the wall-clock budget expired at depth 16"`. Before the fix the same call ran past 1,500 s and wrote nothing. The trace itself still needs the witness search routed through the member that found the row (N5.4).

## Provenance

- **Found by:** the HWMCC'20 all-136 refresh. `circular_pointer_top_w16_d64_e0` with `--timeout-ms 300000 --witness --witness-max-k 200` was killed at 1,500 s with no JSON; without `--witness` the same call returns `violated` at 168 s.
- **Tests:** `the_witness_search_stops_at_the_callers_deadline_and_says_so`, `a_bound_searched_without_a_path_names_the_bound`, `a_budget_spent_by_the_verdict_runs_no_search` (`witness.rs`); `btor2_verify_handler_names_why_a_violation_ships_without_a_trace` (`handlers.rs`).

## Not covered here

- The fault-adversary path (`--fault`, with or without `--target`) re-derives its attack trace with the same unbounded search and drops the miss into a log line. It needs a reason field on `FaultReachOutcome` and is left for a follow-up.
- Routing the re-derivation through the members that found the row (`ric3-bmc`, the owned `sat-bmc` / `ic3`) before native BMC: Track N item N5.4.
