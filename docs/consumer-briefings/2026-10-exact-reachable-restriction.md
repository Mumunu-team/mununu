# Consumer briefing — 2026-10 exact-engine levers from the HWMCC LIVE study: reachable-state restriction, root-fixpoint early exit, exact-rung time slice

> **Audience:** lanes that run the `exact-symbolic` engine (`btor2 verify*`, `sv verify-auto`'s exact member and its ⊥ re-plan, the two-player verbs) and anyone running `btor2 verify-justice --rung both`; orchestrators that pass engine environment variables.
>
> **Related:** [`2026-10-btor2-verify-justice-from-aiger.md`](2026-10-btor2-verify-justice-from-aiger.md) (the verb and the first sweep). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **No default verdict, message or wire shape changes.** Four opt-in environment switches were added to the exact engine (`MUNUNU_BDD_REACH_RESTRICT`, `MUNUNU_BDD_EARLY_EXIT`, `MUNUNU_BDD_COLLECT_AT_BOUND`, `MUNUNU_BDD_REACH_ARENA`) and one to the justice verb (`MUNUNU_JUSTICE_EXACT_BUDGET_MS`). With `REACH_RESTRICT` + `EARLY_EXIT` on, the engine evaluates fixpoints on the reachable sub-structure with care-set-constrained transition functions and stops the root fixpoint as soon as the initial states are decided: `cucab09` (91 bits, HWMCC'17 LIVE, an instance nuxmvlive could not decide in 1 h) goes from an arena abstention at 137 s to HOLDS in 2.6 s; `cuabq2mfro` from 105 s to 4.3 s. Verdict-identical by construction and by test. Two unrelated behaviours changed for everyone: the exact engine's opt-in wall clock is now read every iteration, and the squaring Squarer collects garbage once before refusing on its arena bound.

## What changed

- [`ExactModel::install_reachable_restriction`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) (lever A, `MUNUNU_BDD_REACH_RESTRICT=1`): the forward reachable set `μR. init ∨ Img(R)` is computed off the Squarer's **conjunctively partitioned** relation (`Squarer::forward_reach`, `Squarer::image`: one `s'ⱼ ↔ fⱼ` per state bit with early quantification; the monolithic relation of an 81-latch arbiter does not fit a 33 M-node arena), every set the evaluator forms is conjoined with it, and **every next-state function is Coudert–Madre-constrained to `Reach ∧ constraint`** (`ExactModel::constrain`, used by `to_next`) so a pre-image's intermediate stays inside the reachable space. Sound at every alternation depth: the reachable sub-structure is closed under successors and the verdict reads `init ∧ ¬sat` only. A declined restriction (no next-state functions, the reachable set outgrows the arena, the chain does not close) is logged at `debug` and evaluation proceeds as before.
- [`ExactModel::set_init_hint`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) (lever B, `MUNUNU_BDD_EARLY_EXIT=1`): the root fixpoint (the formula root with its `Not`s stripped) returns as soon as its init-restricted value is fixed — a ν iterate disjoint from `init`, a μ iterate containing it.
- `MUNUNU_BDD_COLLECT_AT_BOUND=1` (lever D): the engine's arena-safety check collects once and judges the live count before abstaining. Measured to decide runs the allocated count would have abstained on, and to let a garbage-dominated fixpoint run to the time budget — pair it with a deadline.
- `MUNUNU_BDD_REACH_ARENA` (default `1 << 25`): the Squarer arena under the restriction. The Squarer now **always** collects once before refusing on its bound (every image / squaring step leaves its predecessor dead).
- `MUNUNU_BDD_REACH_BUDGET_MS` (default `30000`, and never more than a quarter of what is left on the model's deadline): the forward-reach phase's own clock. Past it the restriction is declined and evaluation proceeds unrestricted — the baseline path. Measured need: on `lmcs06dme2p2` the reach phase ran past 200 s, overran the exact rung's whole slice and starved l2s, while the unrestricted fixpoint decides it in 3 s.
- [`ExactSymbolicOptions::time_budget_ms`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + `ExactModel::set_deadline`: a per-request wall-clock deadline for the fixpoints. [`verify_justice`](../../crates/mununu-core/src/adapter/justice.rs) sets it from `MUNUNU_JUSTICE_EXACT_BUDGET_MS` under `--rung both`, so the exact rung leaves the l2s rung its share (the v1 lever sweep lost two l2s lassos to an exact rung that neither converged nor abstained).
- The opt-in wall clock (`MUNUNU_BDD_TIME_BUDGET_MS`) is read **every** iteration instead of every 256th: an iteration of a wide νμ takes seconds, so the stride read it minutes late. The abstention point moves earlier for anyone who had it set; verdicts are unchanged.
- `ExactSymbolicOptions` gained three fields (`reach_restrict`, `early_exit`, `time_budget_ms`); literals without `..Default::default()` need them.

## Measured

| instance (HWMCC'17 LIVE, `--rung exact`) | baseline | A + A2 + B |
|---|---|---|
| `cuabq2mfro` 41 bits `uns` | arena abstention at step 255, 105 s | HOLDS 4.3 s, peak 1.8 M nodes |
| `cucab09` 91 bits `uns` (nuxmvlive, aigbmc failed within 1 h) | arena abstention at step 228, 137 s | HOLDS 2.6 s, 1,102 iterations, peak 856 k |
| `cubakro` 28 bits `sat` | unknown (arena; no l2s lasso) | VIOLATED 1.3 s (reachable set in 77 steps / 100 ms) |
| `arbixs08p03` 56 bits `uns` | > 600 s, no budget fires | arena at step 4 — a large reachable arbiter; not reached by these levers |
| `cucnt32ro` 32-bit counter | iteration budget 4.9 s | same — a 2^32 chain, no squaring rescue for nested νμ |
| gate: `the_levers_never_change_a_justice_verdict`, `the_reachable_restriction_is_the_forward_reachable_set` | — | green; exact-engine module 104/104 |
| v1 sweep (A monolithic + B), ≤192-latch band, 300 s | 52/129 | 55/129, 0 flips, 0 disagreements (6 gained, 3 lost to the exact rung's budget use) |
| final sweep (A + A2 + B, exact rung 150 s) | 52/129 | 61/129 decided (18 HOLDS + 43 VIOLATED) vs 52 at baseline — **0 verdict flips, 0 disagreements** with the published verdicts; 11 gained (`cuabq2mfro`, `cubakro`, `cucab09`–`13`, `cupts14`–`16`, `cutarb4ro` — all by the exact rung in 1–7 s; `cucab11`/`12`/`13` were 300 s timeouts, `cucab13` an instance two published solvers failed on), 2 lost (`lmcs06dme2p1`: an l2s lasso at 258 s that no longer fits the 150 s left after the exact slice; `lmcs06srg5p0`: a Pono proof at 79 s, kept by the previous run and inconclusive in this one — the slice plus solver variance); exact rung 38 decisions, l2s + portfolio 23; median time on the 50 co-decided 1.04×. A first run of this configuration WITHOUT the reach-phase budget scored 60/129 and lost `lmcs06dme2p2` (a 3 s exact decision at baseline) to an unbounded forward-reach phase — the budget recovered it |

## What to update, per consumer

- **Default lanes:** nothing — every switch is off; the only default changes are the every-iteration clock read (earlier abstention only when `MUNUNU_BDD_TIME_BUDGET_MS` is set) and the Squarer's collect-before-refuse (a squaring rescue that used to be refused on dead nodes may now succeed — a decision where there was an abstention).
- **Lanes that want the levers:** export `MUNUNU_BDD_REACH_RESTRICT=1 MUNUNU_BDD_EARLY_EXIT=1`; for `btor2 verify-justice --rung both` also `MUNUNU_JUSTICE_EXACT_BUDGET_MS=<half the lane budget>`. Expect abstentions to turn into verdicts; no verdict flips (gate above).
- **Report parsers:** no shape changes. The `MUNUNU_BDD_REPORT_WORK` line gained three fields (`reach-restricted`, `early exits`, `collections at the bound`).
- **Rust callers building `ExactSymbolicOptions` literally:** add the three fields or use `..Default::default()`.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | engine switches + justice time slice | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** (only if a lane sets the switches) |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- adapter::justice symbolic_bitblast
MUNUNU_BDD_REACH_RESTRICT=1 MUNUNU_BDD_EARLY_EXIT=1 MUNUNU_BDD_REPORT_WORK=1 \
  mununu btor2 verify-justice design.aig --rung exact --json      # the report line shows reach-restricted / early exits
MUNUNU_JUSTICE_EXACT_BUDGET_MS=150000 mununu btor2 verify-justice design.aig --json
```

## Not covered here

- Lever C (a circuit-derived static variable order for 1-bit AIGER latches) and the lockstep fair-cycle algorithm are not in this change.
- The switches are experiment switches: defaults flip only after a full-band sweep with the gate at 0 flips.
