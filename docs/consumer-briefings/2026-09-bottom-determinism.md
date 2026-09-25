# Consumer briefing — 2026-09 a `⊥` now says whether re-running could change it

> **Audience:** monono, ROSF, anyone whose gate branches on `sv verify-auto`'s `unknown`.
>
> **Related:** closes the last open ask of [mununu#553](https://github.com/Mumunu-team/mununu/issues/553). Builds directly on [`2026-09-bottom-reason-and-budget-zero.md`](2026-09-bottom-reason-and-budget-zero.md), which gave a `⊥` its *cause*; this gives it its *determinism*.
>
> **TL;DR:** additive, no verdict changes. Every `⊥` now carries **`determinism`** — `reproducible` · `host-dependent` · `unestablished` — and an engine abstention additionally carries **`budget`** and **`budget_knob`**, so the four budgets that used to share one `kind` are now separable without regexing prose. **You asked for this and said you would use it at once; this is that field.**

## ⚠️ The case worth reading first: `budget-expired` is on by default

`budget-expired` comes from the **harness** clocks — `MUNUNU_PROPERTY_BUDGET_MS` (15 min per
property) and `MUNUNU_VERIFY_BUDGET_MS` (1 h per run). Both are **on by default**, so this `⊥` is
reachable on an ordinary run with nothing configured.

Until now it said nothing about determinism, while the *rarer* `memory-ceiling-exceeded` said
*"Host-dependent without a clock…"*. So the uncommon case read correctly and the common one did
not. Both are now `determinism: "host-dependent"`.

**What that means for a gate:** a `host-dependent` ⊥ is a **configuration result, not a verdict
about the design**. The same command on a quieter or faster machine may decide the property. Do
not pin it as an expected `unknown`, and do not read a red gate from it as a claim about the RTL.

## The new fields

**API** — `properties[].bottom_reason`:

| field | values | what to do |
|---|---|---|
| `determinism` | `reproducible` | a property of the **problem**. Pin the `unknown` honestly, or raise the knob `budget_knob` names |
| | `host-dependent` | a property of the **afternoon**. Treat as a configuration result; re-running may decide it |
| | `unestablished` | not determined. Do **not** read as either |
| `budget` | `iteration` · `node` · `bit-cap` · `arena-safety` · `fixpoint-latency` · `arena-exhausted` · `wall-clock` · `unrecognised` | present for `engine-did-not-complete` only |
| `budget_knob` | e.g. `MUNUNU_BDD_ITER_BUDGET` | the env var to raise, when raising one is the right response |

**CLI** — same information, on the property's own lines:

```
  [assert] video_timing_sva_1: unknown
        bottom-reason [engine-did-not-complete] budget=iteration (MUNUNU_BDD_ITER_BUDGET)
          determinism=reproducible: engine `exact-symbolic` did not complete on this
          design: symbolic bit-blaster: abstained on the ITERATION budget (1048577 >
          1048576) — raise MUNUNU_BDD_ITER_BUDGET
```

## Exactly one engine budget is host-dependent

Of the seven markers the exact engine emits, **only the opt-in wall clock** is a property of the
host. The rest are deterministic total-work bounds:

| `budget` | determinism | raise |
|---|---|---|
| `iteration` | reproducible | `MUNUNU_BDD_ITER_BUDGET` |
| `node` | reproducible | `MUNUNU_BDD_FIXPOINT_NODES` |
| `bit-cap` | reproducible | `MUNUNU_BDD_MAX_BITS` |
| `arena-safety` | reproducible | `MUNUNU_BDD_ARENA_NODES` |
| `fixpoint-latency` | reproducible | `MUNUNU_BDD_FIXPOINT_NODES` |
| `arena-exhausted` | reproducible | `MUNUNU_BDD_ARENA_NODES` |
| **`wall-clock`** | **host-dependent** | `MUNUNU_BDD_TIME_BUDGET_MS` — **off by default since #553**, so you only see this if you set it |

⚠️ **One honest caveat on the node-shaped budgets.** They are reproducible *to about 0.01%*, not to
the byte — a live-node count can differ slightly between runs. A budget set exactly at a cone's
measured peak can still flip. That is a reason to leave headroom when you pin one, not a reason to
treat them as host-dependent.

## Why three values and not a boolean

Because `unclassified-bottom` and an unrecognised engine marker have no established answer, and a
boolean would force one. Guessing `reproducible` there would tell you to pin a `⊥` that may not
reproduce — the same failure in the opposite direction. `unestablished` is the honest value and it
is load-bearing: **treat it as "do not pin, do not retry-loop"** rather than as a synonym for
either neighbour.

The engine's error channel is a `String`, so an unrecognised marker is a real possibility whenever
a message is reworded. It fails to `unestablished` rather than to a default.

## Test the transition

```bash
# every host-dependent bottom, without string matching
mununu --quiet sv verify-auto design.sv --json \
  | jq '[.properties[] | select(.bottom_reason.determinism == "host-dependent")
         | {property: .name, kind: .bottom_reason.kind}]'

# the ones you can honestly pin
mununu --quiet sv verify-auto design.sv --json \
  | jq '[.properties[] | select(.bottom_reason.determinism == "reproducible")
         | {property: .name, budget: .bottom_reason.budget, raise: .bottom_reason.budget_knob}]'
```

If either needs a regex over `detail`, tell us — that is the defect this change exists to remove.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

A consumer pinning a mununu commit needs the new commit to see the fields; nothing else changes.

## Not covered here

- **The per-property note join.** Notes still live at report level with no link to the property
  they explain ([mununu#548](https://github.com/Mumunu-team/mununu/issues/548)'s larger half). It
  needs a wire-format decision and lands with
  [mununu#541](https://github.com/Mumunu-team/mununu/issues/541).
- **The two harness clocks are still on by default.** #553 turned off the *engine* clock
  (`MUNUNU_BDD_TIME_BUDGET_MS`); `MUNUNU_PROPERTY_BUDGET_MS` and `MUNUNU_VERIFY_BUDGET_MS` remain
  at 15 min and 1 h. They are now *labelled* host-dependent rather than silent, which is what this
  change delivers — not removed.
- **The planner's decidability prediction** is still emitted and is still
  [unreliable at the cap](https://github.com/Mumunu-team/mununu/issues/548); treat it as telemetry.

---

**Provenance.** Issue: [mununu#553](https://github.com/Mumunu-team/mununu/issues/553) ask 2 (and
the residual half of ask 1). Policy:
[`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
