# Consumer briefing — 2026-09 `sv verify-auto` now has wall-clock budgets, ON by default

> **Audience:** monono (reported the incident), ROSF, anyone running `mununu sv verify-auto` in a gate.
>
> **Related:** [mununu#504](https://github.com/Mumunu-team/mununu/issues/504). Follows the C1–C4 machinery (#520, #521, #522, #523).
>
> **TL;DR:** a run had **no time bound at any granularity**. It does now: **15 min per property, 1 h per run**, both on by default. On expiry the remaining properties abstain as `unknown` and **every verdict already computed is preserved** — a run that used to hang, or be killed with no output at all, now reports what it decided. Set either variable to `0` to restore the old unbounded behaviour.

## What changed

| | Before | After |
|---|---|---|
| per property | unbounded | **15 min** (`MUNUNU_PROPERTY_BUDGET_MS`) |
| whole run | unbounded | **1 h** (`MUNUNU_VERIFY_BUDGET_MS`) |
| lift subprocesses | unbounded | 15 min (`MUNUNU_LIFT_TIMEOUT_MS`, shipped in C2) |

`0` disables any of them.

## Why these numbers — measured, not guessed

Per-property wall time over the real-design e2e corpus (`MUNUNU_PROPERTY_TIMING=1`, n = 122):

| p50 | p90 | p99 | max |
|---|---|---|---|
| 2.2 s | 14.7 s | 41.3 s | **49.1 s** |

The default is **15 minutes — about 18× the observed maximum**, deliberately far above the usual
"3× p99" rule. Two reasons the numbers alone don't show:

1. **That corpus is the easy case.** It is our own e2e set; your designs are larger. Tuning a
   default to it would be tuning to what we already handle comfortably.
2. **The failure modes are not symmetric.** Too high means a hang takes longer to catch —
   annoying. Too low means mununu abstains on work that was *succeeding*, and a budget abstention
   is `unknown`, which **fails a strict gate**. That would turn currently-green gates red for
   properties that were fine. So: err high.

**The run budget is the one that addresses the reported incident.** A per-property cap alone
would not have helped — 42 properties × 15 min is still 10 hours worst case. An hour-long *run*
budget turns a six-hour loss into a report after an hour with the undecided tail marked `unknown`.

## What you will see

A `time-budget-exceeded` note, and `unknown` for every property not reached:

```
note: time-budget-exceeded — wall-clock budget reached; the remaining properties
      abstained rather than hanging (MUNUNU_PROPERTY_BUDGET_MS / MUNUNU_VERIFY_BUDGET_MS)
```

**Abstentions are `unknown`, never `skipped`.** That is deliberate: `ci_exit_code` does not fail
on `skipped`, so classifying a timed-out property as skipped would let a strict
`--fail-on unknown` gate pass **green on a property that was never decided**. A silent pass is
worse than a red gate.

## Will this change my verdicts?

**Only if a property currently takes longer than the budget.** If your run completes today inside
15 min/property and 1 h/run — and the corpus above suggests that is a wide margin — nothing moves.

If something does move, it moves `holds`/`violated` → `unknown`, never between definite verdicts.

**If a gate goes red after this, that is the intended signal, not a regression**: it means a
property genuinely exceeded the budget. Raise the variable, or investigate why that property is
slow — `MUNUNU_PROPERTY_TIMING=1` prints per-property wall time and will tell you which one.

## New diagnostic

```bash
MUNUNU_PROPERTY_TIMING=1 mununu sv verify-auto …
# [mununu-timing] property=foo_sva_3 outcome=holds elapsed_ms=2212
```

One line per property. This is how the defaults above were chosen, and it is the tool for tuning
them for your designs. Off by default, zero cost when unset.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | new default time bounds | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no verify path | No |
| rosf | runs verify-auto | **Yes** |
| monono Docker | reported the incident; gate runs verify-auto | **Yes** |
| mununu-ui | no type change | No |

## Verification

```bash
cargo test -p mununu-core --lib -- run_budget shipped_defaults
```

The defaults are pinned by a test, with an invariant that the run budget must comfortably exceed
a single property's — otherwise one slow property consumes the whole run.

The pre-merge gate is the e2e suite run **with defaults on**, asserting no property changes
verdict, in the `mununu-sva` image.

## Not covered here (follow-ups)

- **The memory ceiling default** (C6) is separate and still opt-in as of this change. It will
  supersede the #490 briefing's *"unset ⇒ disabled"* line when it lands.
- **A SIGKILL-survivable partial report** (C7). These budgets degrade gracefully in-process; they
  do nothing if the gate `kill -9`s you or the OOM killer fires.
