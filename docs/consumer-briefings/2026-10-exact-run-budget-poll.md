# Consumer briefing — the exact engine honours the caller's run budget (mununu#804, 2026-10)

> Source of truth: [`charge_iteration`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + [`run_budget::expired`](../../crates/mununu-core/src/adapter/run_budget.rs) — surface: (CLI+API) — every verb that runs the exact engine under `MUNUNU_PROPERTY_BUDGET_MS` / `MUNUNU_VERIFY_BUDGET_MS`, a rescue slice or a harness budget

**Audience:** rosf, monono and anyone who runs mununu under a wall-clock budget and reads `bottom_reason` / the abstention kind.

## TL;DR

- **Behaviour change, budgeted runs only.** The exact engine (`exact-symbolic`, OxiDD ROBDD) now polls the caller's run budget (`run_budget`: the planner's per-property budget, the whole-run budget, a rescue slice, the HWMCC harness's per-instance budget) every fixpoint iteration, every squaring step and every reach-probe step. Until now it polled only the opt-in `MUNUNU_BDD_TIME_BUDGET_MS` deadline, so a run that had spent its budget inside a wide fixpoint kept going — `vis_arrays_bufferAlloc` ran four hours under a 300 s budget (one OxiDD collection of ~53 M nodes every ~50 s) and reported `Unknown` with no word about the budget.
- **What you see instead:** the row abstains within the budget with the `deadline` abstention kind and the message `abstained on the caller's RUN budget … with the fixpoint still iterating at N steps`; the planner's `budget-expired` bottom reason is unchanged. A row the exact engine DECIDED before is decided the same way — the poll fires only after the budget has passed, when the old code was already overrunning.
- **Unbudgeted runs are unchanged.** No budget installed ⇒ the poll never fires; the verdict-identity gate (census + ledger) is the proof.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a budgeted row that used to overrun its budget now comes back `unknown` inside the budget with `deadline` | none; if you gated on wall time, you can now trust the budget |
| **monono** | same | none |
| **mununu-ui** | none (RTL verbs) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the poll lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | — |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | — |

## Measured

| run | result |
|---|---|
| `the_callers_run_budget_stops_the_fixpoint_with_a_deadline_abstention` (new; the reproducer) | both tests pass on the host (0.24 s): the 50 ms budget stops the raster fixpoint with `Deadline`; an unbounded budget never fires the poll (`tests/exact_run_budget.rs`) |
| the reproducer against the parent `7755580`, by hand (a worktree at the parent with `tests/exact_run_budget.rs` copied in; `make verify-repro` reads INCONCLUSIVE because it runs lib tests only) | **FAILS at the parent** — `the_callers_run_budget_stops_the_fixpoint_with_a_deadline_abstention` panics at its first assertion (no run-budget abstention; the run ends on the iteration budget after 1.5 s); passes on this commit |
| HWMCC two-file harness (`vis_arrays_buf_bug` + `vis_arrays_bufferAlloc`, `MUNUNU_HWMCC_TIMEOUT_MS=60000`) in `mununu-sva` | **100 s for both files** (`vis_arrays_buf_bug` Reachable by exact + btormc; `vis_arrays_bufferAlloc` Unknown inside ~1.6× the 60 s budget) — `main` took 15,015 s on the same pair |
| verdict-identity gate: `e2e_opentitan_sva_census` vs `main` and `diff_corpus_monotone_verdict_ledger` | **0 verdict moves** — 191 rows, 173 holds / 7 violated / 11 skipped / 0 ⊥ on both (2,499 s); ledger PASS (22 s) |

## Provenance

- Issue: mununu#804 (class `silent`, `found:hwmcc`); siblings #726 (the rescue's fixed query), #728 (the predicate-cube loops).
- Policy: [`docs/policies/failure-paths.md`](../policies/failure-paths.md) Rule 1, [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The bit-blasting phase itself (building the transition relation) is not polled; it is bounded by the bit cap, not by time. Polled since mununu#816: see [the follow-up briefing](2026-10-exact-run-budget-steps-and-owned-driver.md).
- Folding the run budget into the exact model's `deadline` (which would also enable the deadline-gated collection lever) — deliberately not done, to keep budgeted verdicts otherwise identical.
