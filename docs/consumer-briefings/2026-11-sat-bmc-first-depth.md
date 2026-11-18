# Consumer briefing — the owned SAT BMC's counterexample is the shortest one again (mununu#1057)

> **Audience:** maintainers of the repositories that read a `violated` row's counterexample from `mununu sv verify-auto` (the slice, `--localize`), `mununu btor2 verify --witness`, or the HTTP routes behind them (rosf, monono, mununu-ui). Not a release note.
>
> Source of truth: [`dynamic_search`](../../crates/mununu-core/src/adapter/btor2/sat_bmc.rs#L327) · [`DynamicStep`](../../crates/mununu-core/src/adapter/btor2/sat_bmc.rs#L67) — surface: CLI+API

## TL;DR

The owned SAT BMC member (`sat-bmc`) searches in doubling windows of depths and asks one query per window: "any `bad` in [lo, hi)". The model it got back was taken at face value, so the trace it shipped was the path the solver found first, not the shallowest one in the window: the `sv verify-auto` slice fixture's 4-step counterexample came back as 7 steps from the window [3, 7), deterministically, on every run since #1050 made `sv verify-auto`'s safety witness use the trace-carrier search (where `sat-bmc` is first in the fixed order). The scheduled e2e run on `3d1c235a` caught it (mununu#1057).

Now a hit is shrunk in place: the member keeps asking, under the same conflict budget, for any `bad` strictly below the depth it has, until UNSAT proves the depth is the first one (every depth below the window was refuted before it was encoded). What changes for a consumer:

1. **Counterexamples from `sat-bmc` are the shortest ones**, as native BMC's are. A row whose trace `sat-bmc` carried may now be shorter than before; the verdict, the fields and the route are unchanged. The slice and `--localize` read the trace's cycles, so their cycle numbers follow.
2. **A trace carried by another member is unchanged.** `ic3`, btormc and rIC3's traces are their own and were never shortest; the portfolio ships the first replayed trace in the fixed engine order, `sat-bmc` first when it has finished (`docs/consumer-briefings/2026-11-witness-carried.md`).
3. **Cost:** one or two extra incremental SAT queries per hit on the frames already encoded. A shrink query that runs out of its conflict budget keeps the depth it has: the trace is still a genuine counterexample, only its minimality is unestablished, and the `sat-bmc` log says so (`RUST_LOG=mununu_core::adapter::btor2::sat_bmc=warn`). No report field changes.

Every `sat-bmc` counterexample is still replayed on the concrete simulator before it ships.

## Per consumer

### rosf · monono (CLI and HTTP)

- A `violated` row's `counterexample` (and `trace` on `btor2 verify --witness`) may have fewer steps than before when `sat-bmc` carried it. A consumer that stored a trace's length as an expected value re-baselines it; one that reads the trace's cycles (slice, localisation) sees the earlier cycles.
- No response field, route, flag or verdict value changes; `docs/api-schemas/` untouched.

### mununu-ui

No CTXDSL surface; no change required.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the member's search |
| `mununu-sva` (dev / e2e) | yes | same; the two e2e tests of mununu#1057 pass here again |
| consumers' own images | no | no schema change |

## Provenance

Measured 2026-11-18 in `mununu-sva` on main `36124c03`: the slice fixture's trace is 7 steps on 6 of 6 runs, carried by `sat-bmc` (3 of 3 probed runs, with `ic3`, btormc and rIC3 also reporting reachable), unchanged by `MUNUNU_REACH_IC3=0` (which governs the decider sets, not the trace carriers). The issue's hypothesis (an `ic3` trace) was falsified by the probe. The seam test `the_dynamic_step_reports_the_first_depth_not_the_first_path_it_found` (two counters on one input, violations at depths 3 and 6 in the same window, both input polarities) fails with only its test module spliced onto the parent's file and passes on the fix; the two e2e tests are the end-to-end detectors.

## Not covered here

Whether the portfolio should prefer a shortest-trace carrier over a faster deeper one (the issue's first option) is not decided here: `sat-bmc` wins the fixed order when it finishes, and a deep trace from `ic3` or rIC3 where `sat-bmc` did not finish is still the trace that ships, as documented since N5.4. rIC3's own `--bmc-dyn-step` traces (`ric3-bmc`) have the same window property and are not changed by this.
