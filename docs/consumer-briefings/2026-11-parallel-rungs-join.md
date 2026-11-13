# Consumer briefing — `--parallel-rungs` returns when the first rung wins (2026-11)

> Source of truth: [`verify_justice_with`](../../crates/mununu-core/src/adapter/justice.rs#L485), [`ReachMembers::CANCELLABLE`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L554) and [`ReachMembers::PROOF_DIRECTED_RACED`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L549) — surface: CLI+API (`mununu btor2 verify-liveness --justice --parallel-rungs`, `POST /api/v1/btor2/verify-liveness` with `parallel_rungs: true`).

> **Audience:** anyone who passes `--parallel-rungs` / `parallel_rungs: true` to the justice verb — rosf and monono (CI gates in the `mununu-sva` image), and agents on the HTTP API. If you never pass it, nothing in this briefing affects you.
>
> **TL;DR — a bugfix in the opt-in parallel policy, plus a measurement that says not to use it on a small host.** The parallel policy waited for the l2s rung's exact reach member to run out its own budget even after another rung had won (#986): a row the exact rung decided in 0.4 s took 261 s. Now the l2s rung runs without that member in the race, and k-liveness runs rIC3 as one `-e ic3` process instead of its ~11-solver portfolio. Some rows that ended `timeout` / `unknown` now decide; no decided verdict changes value. **Even fixed, at 4 CPUs the parallel policy decides fewer rows than the default sequential order** (90 vs 100 of 129). The default (`--rung both` without `--parallel-rungs`) is unchanged.

## What changed

| path | before | now |
|---|---|---|
| default `--rung both` (sequential) | — | unchanged, byte for byte: same members, same order |
| `--rung exact` / `l2s` / `kliveness` alone | — | unchanged |
| `--parallel-rungs`, exact rung wins first | the call returned only when l2s's exact member hit its budget (up to the whole budget) | returns at once; the exact rung's notes are kept in the output (they were dropped) |
| `--parallel-rungs`, l2s rung | reach portfolio with the exact member | reach portfolio without it (btormc, Pono, rIC3, native, interpolation) |
| `--parallel-rungs`, k-liveness rung | rIC3 portfolio mode | rIC3 `-e ic3` |

The JSON shape is unchanged. The `outcome` value can move from `timeout` / `unknown` to a definite verdict, or the other way (a row whose l2s proof needed the exact member, or the cores now shared), and `decided_by` can name a different rung.

## Measured (HWMCC'17 LIVE, 129 rows, `mununu-sva`, `--cpus 4`, 300 s, one row at a time)

| arm | holds | violated | unknown | timeout | decided |
|---|---|---|---|---|---|
| sequential `both` (default, unchanged) | 42 | 58 | 19 | 10 | 100 |
| `--parallel-rungs`, before (main) | 34 | 43 | 38 | 14 | 77 |
| `--parallel-rungs`, after the join fix only | 35 | 46 | 42 | 6 | 81 |
| `--parallel-rungs`, after both fixes | 38 | 52 | 31 | 8 | 90 |

(The "before" row is run 1 on an earlier `main`; its sequential arm read 41 H / 58 V.)

After both fixes, compared with the sequential order: 13 rows lost (all `l2s+ric3` / `l2s+btormc` rows that take 80–145 s sequentially), 3 gained (`cuhanoi10ro`, `cutf3ro`, `lmcs06dme2p1`), **0 verdicts disagree**, and 10 holding rows are proved by k-liveness. Rows the exact rung decides alone run a median 5.7× slower under the race, since the rungs' solvers share 4 cores. Summed wall time: 233 min parallel vs 223 min sequential.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | If a gate passes `--parallel-rungs`, rows that timed out may now decide. Some rows may also go back to `unknown`. | Drop `--parallel-rungs` on runners with ≤ 4 cores; the default decides more rows. |
| **monono** | Same, through rosf. | None beyond rosf's. |
| **mununu-ui** | No surface. | None. |
| **HTTP API agents** | Same as rosf, for `parallel_rungs: true`. | Omit `parallel_rungs` unless you have measured it on your host. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images | Yes, to get the fix. | A binary change in the justice verb's parallel policy. |
| rosf `docker/Dockerfile*` | No. | rosf runs the mununu image. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

- #986. The join defect has an in-repo reproducer, `adapter::justice::tests::the_parallel_policy_returns_when_the_exact_rung_wins_without_waiting_for_l2s`: it fails on the parent (36.6 s, notes empty) and passes on the fix (< 1 s).
- The rIC3 change is a CPU-contention measure. It has no unit-test reproducer; the evidence is the 129-row runs above and a 16-row probe (4 of the 7 rows lost after the join fix came back under `-e ic3`).
- Run data is kept privately with the other competitiveness runs.

## Not covered here

- Hosts with more than 4 cores. A 16-row probe at 8 CPUs recovered 5 of the 7 lost rows, but the full set was not run there.
- The response verbs' k-liveness fallback (V2). It runs sequentially and keeps rIC3's portfolio mode, so it is unchanged.
- The exact reach member's missing cancellation point (#816). This fix stops waiting for the member in the race; it does not make the member cancellable.
