# Consumer briefing — 2026-10 the exact engine's ladder: lever switches removed, early exit on by default, `verify-justice --budget-ms` / `--parallel-rungs`

> **Audience:** lanes that set `MUNUNU_BDD_REACH_RESTRICT`, `MUNUNU_BDD_EARLY_EXIT`, `MUNUNU_BDD_COLLECT_AT_BOUND` or `MUNUNU_JUSTICE_EXACT_BUDGET_MS` (all introduced one day earlier in [`2026-10-exact-reachable-restriction.md`](2026-10-exact-reachable-restriction.md)); everyone running the `exact-symbolic` engine; `btor2 verify-justice` callers and the `/api/v1/btor2/verify-justice` route.
>
> **Related:** the lever gating plan (`.claude/plans/lever-gating-plan.md`). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Four environment switches are gone; what they switched is now derived or scheduled.** The root fixpoint's early exit is on for every verdict path (one conjunction per iteration; the verdict reads `init ∧ ¬sat` only). The reachable-state restriction is a stage of the exact engine's **ladder** — unrestricted for a short slice, then restricted with a 5 s probe on a retryable abstention, then unrestricted with the remainder — applied in `verify-justice` and in the ⊥ re-plan's recoverability rescue, not as a global. Collect-before-abstain on the arena bound happens whenever a deadline is set. `verify-justice --rung both` is `exact ladder ≤ 30 s → l2s → exact ladder remainder`, with `--budget-ms` (per justice set) and `--parallel-rungs` (both at once, first definite wins). No wire shape changed; two request fields and two CLI flags were added.

## What changed

- [`ExactSession`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) (P2): the per-property session — every rewrite, the cone, the bit-blast, the atoms and the legal initial states built once; `attempt(&AttemptOptions)` evaluates with per-attempt options and a deadline. The one-shot verdict functions are `prepare` + one `attempt` (byte-identical results).
- [`exact_ladder`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) (P3): the three stages above on one session; `MUNUNU_EXACT_LADDER_MS` (default 2000) is the first slice. A stage's abstention is kept in the outcome's notes; `LadderOutcome::stage` is the `decided_by` suffix (`exact-symbolic` / `exact-symbolic (reach-restricted)`).
- [`AbstentionKind`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + `abstention_kind` (P0): the typed reading of an abstention message — `BitCap`, `ArenaSafety`, `IterationBudget`, `Latency`, `Deadline`, `Cancelled`, `RefusedInputAtom`, `ReachDeclined`, `ArenaOverflow`, `Other` — with `is_retryable`. Messages are unchanged; the planner keys on the kind.
- `ExactSymbolicOptions`: `early_exit` defaults to **`true`**; `reach_restrict` defaults to `false` and is no longer read from the environment; `time_budget_ms` is the ladder's whole clock on the recoverability rescue.
- `ExactModel`: `set_deadline_opt` (the process-wide `MUNUNU_BDD_TIME_BUDGET_MS` stays a floor), `cancel_handle` / `set_cancel` (a concurrent rung cancels the fixpoint; the abstention reads `cancelled by the caller`), `clear_restriction`, `clear_init_hint`, `reset_counters`; the arena-safety collection is derived from `deadline.is_some()`.
- P5: the Squarer is capped at `3 · 1024 + 1024` variables (a typed `reach:` decline past it).
- [`verify_justice_with`](../../crates/mununu-core/src/adapter/justice.rs) + `JusticeOptions { rung, budget_ms, parallel_rungs }`; `verify_justice(btor2, rung)` stays as the sequential, unbudgeted wrapper.
- [`verify_recoverability_attributed`](../../crates/mununu-core/src/adapter/recoverability.rs): the exact arm runs the ladder; `RecoverabilityEngine::ExactReachRestricted` (`decided_by` "exact-symbolic, reach-restricted (recoverability rescue)"; `sv verify-auto`'s rescue note says "exact+COI, reach-restricted").
- CLI `btor2 verify-justice --budget-ms <MS> --parallel-rungs`; API `Btor2VerifyJusticeRequest { budget_ms?, parallel_rungs? }`.
- `scripts/hwmcc_live_study.sh`: `EXTRA_ARGS` appended to the verb's command line.

## Verdict vocabulary

Unchanged. A new `decided_by` value can appear: `exact-symbolic (reach-restricted)` (justice) and `exact-symbolic, reach-restricted (recoverability rescue)` (verify-auto's rescue).

## Measured

| | result |
|---|---|
| `lever_gating_tests` (P0 classification table; the ladder's first stage; the forced fall-through to the restricted stage with the same verdict; a zero budget; a cancelled session) | 5/5 — the fall-through test caught a real bug on the first run (the reach probe inherited the previous stage's expired deadline and declined at step 0; the attempt now sets its deadline first) |
| `justice::policy_tests` (sequential policy names the stage; parallel policy decides and cancels; a zero budget abstains) | 3/3 |
| exact-engine module, justice module; the full `mununu-core` lib suite; the six two-player integration crates | 109/109, 10/10; **2,871 passed / 0 failed** (87 ignored = the image-gated e2e set); 15/9/4/6/7/2, 0 failures |
| wall-class matrix (`mununu-sva`) | MEASURED-PLACEHOLDER |
| HWMCC'17 LIVE ≤192 latches, 300 s, default `--rung both` (no switches) | **64/129 decided (19 HOLDS + 45 VIOLATED) vs 52 at baseline (+12) — 0 verdict flips, 0 disagreements, 0 lost**; gained the eleven exact-rung instances (`cuabq2mfro`, `cubakro`, `cucab09`–`13`, `cupts14`–`16`, `cutarb4ro`, 3–20 s) plus `cutq2ro` (l2s + btormc at 68 s, a 300 s timeout before); against the switch-driven sweep of #674 (61/129) it is +3 with 0 lost — the three l2s rows the fixed exact slice had cost (`lmcs06dme2p1`, `lmcs06srg5p0`, `cutq2ro`) are back; `decided_by` names the stage (24 `exact-symbolic`, 13 `exact-symbolic (reach-restricted)`, 27 l2s + portfolio); median time on the 52 co-decided 0.99× |
| heat-check corpus (`heat_check.py`, main `44bf4b9` vs this branch, early exit on by default) | work and iteration counters identical on every case (raster 16,410 / 4,098; twocount 16,409 / 4,098; forward 16,402 / 4,097; i2c 17 / 4; relational 22 → 23 BDD ops), wall within noise (raster 2.41 → 2.15 s, the rest ±1 %) — the default flip costs nothing measurable |

## What to update, per consumer

- **Lanes that exported the four switches:** remove them; the behaviour they enabled is now the default (early exit), scheduled (the restriction, via the ladder in `verify-justice` and the rescue), or derived (collect-at-bound from the deadline). A lane that wants the restriction on a one-shot `btor2 verify` call sets `ExactSymbolicOptions::reach_restrict` through the API's options — there is no environment switch for it.
- **`verify-justice` callers:** `--rung both` now runs the ladder for 30 s before l2s and returns to the exact ladder afterwards; pass `--budget-ms` to bound a set and `--parallel-rungs` where cores allow (the losing rung's subprocesses run out on their own timeouts).
- **Report parsers:** no shape changes; the new `decided_by` values above.
- **Rust callers building `ExactSymbolicOptions` literally:** no new fields (the two from #674 remain; `early_exit` now defaults to `true`).

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | engine default + verb options | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** (if a lane set the switches or uses `verify-justice`) |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- lever_gating_tests adapter::justice symbolic_bitblast
mununu btor2 verify-justice design.aig --json                       # ladder 30 s → l2s → ladder
mununu btor2 verify-justice design.aig --budget-ms 300000 --parallel-rungs --json
```

## Not covered here

- The first-pass `sv verify-auto` exact member still makes one unrestricted attempt; the ladder moves there only after the corpus timing (plan P4).
- Lockstep / inner-EU squaring, lever C, IC3 on the l2s monitor (plan P6).
