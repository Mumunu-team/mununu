# Consumer briefing — the ⊥ re-plan runs under the per-property budget and a rescue slice; the cube polls its budget (2026-10)

> Source of truth: [`escalate_bottom_with_slice`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`member_timeouts`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) + [`rescue_slice_ms`](../../crates/mununu-core/src/adapter/run_budget.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone gating on `sv verify-auto` verdicts or parsing `[mununu-timing]` lines. Issues mununu#726, mununu#728.

## TL;DR

- **More rows decide, none flip.** The ⊥ re-plan's reach portfolio used to run its native BMC / k-induction under a fixed 5-s per-query timeout, whatever your `MUNUNU_PROPERTY_BUDGET_MS`; on a 200-register IP that let one to three of eight identical rows decide per run. The members now take their time from the budget. Every definite verdict you had stays; rows that were `unknown` may now be `holds` / `violated`.
- **New knob `MUNUNU_RESCUE_SLICE_MS`** (default 60 s, `0` disables): the wall each portfolio-backed rescue step may spend per row. A child of `MUNUNU_PROPERTY_BUDGET_MS`, which is a child of `MUNUNU_VERIFY_BUDGET_MS`. 60 s is the wall the portfolio already ran under, so a run with btormc installed sees no change in its worst case; an undecidable safety row that used to cost ~15 s may now cost up to 60 s.
- **A per-property budget expiry no longer abstains every later row.** Only `MUNUNU_VERIFY_BUDGET_MS` stops the run. The expired row carries `bottom_reason: "budget-expired"` (it used to be overwritten by the shape classifier); its `outcome` is `unknown`, never `skipped`.
- **One wide invariant can no longer run 26 min under a 5-min budget.** The predicate-cube pipeline polls the budget in its label, feasibility, post-image, hyper-must and refinement loops and in the symbolic member.
- **`[mununu-timing]` gains trailing fields**: ` phase=main` on the existing line, and a new line per re-plan row: `… phase=replan lane=<safety|liveness|recoverability|none>`. Prefix parsers keep working; a fixed-column parser does not.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (formal gate) | rows that were `unknown` after the re-plan may decide; `budget-expired` abstentions are now attributed per row and the run no longer stops on one slow property | rerun the gate; if you pin `unknown` rows with `--expect NAME=unknown`, expect pins to break where rows now decide — that is the point |
| **monono** (`make formal`) | same; a row that hit the budget shows `bottom_reason: budget-expired` and the later rows are attempted (your six-hour-hang protection is `MUNUNU_VERIFY_BUDGET_MS`, unchanged at 1 h) | rerun; raise `MUNUNU_RESCUE_SLICE_MS` only if a deep counterexample needs more than 60 s of BMC per row |
| **mununu-ui** | no JSON shape change; `bottom_reason` values unchanged; `decided_by: "reach-rescue"` now appears on rows the safety rescue decided (it was absent before) | none required |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` | yes | the verdict change lives in the binary |
| `mununu-sva-pono` | yes | same; the Pono / btormc members now take the slice as their wall |
| `mununu-ui` | no | consumes the JSON unchanged |

## Measured

OpenTitan `edn` (`examples/verify/ip_opentitan_edn`, 200 registers, `mununu-sva`, shipped defaults), before → after:

| | before (`fb648c8`) | after |
|---|---|---|
| the IP's 52 assertions | 23 holds / **1–3** violated / 20–18 ⊥ / 8 unsupported, load-dependent | 23 holds / **8** violated (each with its trace, `decided_by: reach-rescue`, 20–29 s a row) / 13 ⊥ / 8 unsupported — **0 definite flips** |
| wall | 330 s | 584 s (the sixteen rescue decisions; the 13 residual ⊥ rows cost 0 ms — the rescue declines their shapes) |
| the 26-min invariant (`edn_fatal_alert_has_a_source`, 5-min property budget) | killed from outside after 26 min, no verdict, no reason | the `symbolic` member stops at 308 s with `bottom_reason: budget-expired`; the rescue decides the row VIOLATED in 7 s (a gap in the hand-written property, confirmed by the trace) |
| pattgen / csrng | 14/14 · 20/14/3 | identical |
| `e2e_opentitan_ip_edn_full` (now pins all eight rows) | 561 s | 937 s |
| `e2e_opentitan_ip_edn_stick_flips_the_error_record` | 1 real flip + 4 spurious violated↔unknown "flips" | 1 real flip, 0 spurious |
| wall-class matrix | — | soundness assertion holds (2 tests, 26 s); every cell matches the recorded post-#675 table (`trap_uf_w48` VIOL / ⊥ / ⊥ / SKIP / VIOL, `staller` VIOL, `crossed_vector_field` HOLDS / ⊥ / ⊥ / SKIP / HOLDS, every decidable case decided) — no lever moved, as a budget change must not |

## Not covered here

- `MUNUNU_BDD_TIME_BUDGET_MS` (the exact engine's own clock) is unchanged and still off by default.
- The final ∃-quantification inside the cube's BDD post-image and the 3-valued game evaluation are not interruptible; a poll fires right after each.
- The cancellation latency of a long native BMC query is bounded by the slice, not by a peer's decision (a z3 interrupt watcher is a follow-up).
