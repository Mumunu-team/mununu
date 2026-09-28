# Consumer briefing — 2026-09 the planner's "decidable" prediction was a tautology, and no longer claims to be a forecast

> **Audience:** monono — this is your ask 26, the half that was left open. Anyone reading `plan-accuracy` or `plan-cost` verification notes.
>
> **Related:** closes cause (b) of [mununu#548](https://github.com/Mumunu-team/mununu/issues/548). Cause (a) — the mis-scoped abstraction-posture note — shipped earlier in `cc28d9c`.
>
> **TL;DR:** **note-text change, no verdict changes, no schema change.** The planner never predicted decidability; it reported admissibility and called it a prediction. `plan-accuracy` and `plan-cost` now say "routed" / "ADMISSIBLE" instead of "predicted (decidable)" / "decides definitely". `verification_notes[i].kind` is unchanged, so a gate keying on `kind` needs nothing.

## What you reported

```
[assert] ann_guarantee_0: UNKNOWN/⊥ (0 cell(s))
· ann_guarantee_0: predicted `exact-symbolic` (decidable), outcome ⊥
```

on a 127-bit cone against a 127-bit cap. You filed it as one issue because you could not tell from outside whether the note or the prediction was wrong.

**Neither was wrong, and that is the finding.** The planner was reporting *admissibility* — may the exact engine be admitted at all — and printing it as a forecast. A reader reasonably took "(decidable)" as a claim about whether it would converge.

## Why the comparison could not have told you anything

Two reasons, and the first is a proof rather than a measurement.

**1. With no `MUNUNU_BDD_MAX_BITS`, the comparison is a tautology across the whole default band.** The cap is `max(40, min(cone, 192))`. So for every cone between 40 and 192 bits, **`cap == cone_bits` by construction** and `cone_bits <= cap` is unconditionally true. `fits` is really `cone_bits <= 192`.

Your 127-against-127 is not a boundary case — it is what that band always looks like. The rationale string *"cone 127b ≤ 127b cap"* invited you to read a comparison that had already been decided by the cap's own definition.

**2. Width does not bound convergence, and mununu's own source says so with numbers.** `AUTO_CAP_CEILING`'s doc records `twocount32`: a **65-bit** cone — smaller than i2c's decidable 173 — whose `EF` fixpoint grinds ~1M preimages for **≈132 minutes**. Its conclusion, verbatim:

> So the cap can NOT be the tractability gate — the `ExactModel::deadline` wall-clock backstop is.

And that backstop has been **off by default since [#553](https://github.com/Mumunu-team/mununu/issues/553)**, because a wall clock makes a verdict host-dependent. So the gate the cap's own reasoning names as the real one no longer runs unless you ask for it.

What actually bounds convergence is `MUNUNU_BDD_ITER_BUDGET` and `MUNUNU_BDD_FIXPOINT_NODES` — neither a function of cone width, and deciders and non-deciders overlap **6.2×** in node count.

## What changes in the output

| before | after |
|---|---|
| `predicted `exact-symbolic` (decidable), outcome ⊥` | `routed to `exact-symbolic` (admissible — not a forecast), outcome ⊥` |
| `planner cost prediction: 2/3 properties matched (predicted-decidable vs actually-decided)` | `planner routing vs outcome: 2/3 properties were decided by a routed engine (routing is ADMISSIBILITY, not a decidability forecast)` |
| `cone 127b ≤ 127b cap → exact decides `bad`-reachability definitely` | `cone 127b ≤ 127b cap → exact is ADMISSIBLE for `bad`-reachability; convergence is set by the iteration / node budgets, which the width does not predict` |

The divergence detail now says what a divergence means:

> A property routed to an engine that returned ⊥ is **NOT** a failed prediction, because no decidability was predicted: the routing says the engine was admissible on cone width, and width does not bound convergence. Read the property's own `bottom_reason` for why it did not converge.

**`verification_notes[i].kind` is unchanged** (`plan-accuracy`, `plan-cost`). Only the human-readable `summary` / `detail` / `items` text moves. If you match on those strings, re-baseline; if you key on `kind`, nothing to do.

## What this does NOT do

**It does not make the prediction better.** A genuine decidability forecast would need a model of fixpoint convergence, and the evidence above says width is not it. Removing a false claim is the whole change — the honest note is more useful than a confident wrong one, and pretending otherwise is what produced your self-contradicting run.

So: **treat routing as "the engine we try first", and the property's own `bottom_reason` as the answer to "why didn't it decide".** Since [#553](https://github.com/Mumunu-team/mununu/issues/553) that reason names its budget and whether re-running could change it.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **mununu#548's O-2, the per-property note join.** Notes still live at report level with no link to the property they explain; the join is still `summary.starts_with("<name>: ")`. It needs a wire-format decision and lands with [mununu#541](https://github.com/Mumunu-team/mununu/issues/541), so **#548 stays open** for it.
- **`diameter_log2` is consulted for Liveness only.** Safety / Reachability / Mixed / Propositional ignore it entirely. Unchanged here, and worth knowing if you read a `plan-cost` note on a safety property and wonder why no counter is mentioned.
- **Making exact converge more often.** A different track; this is about not claiming it will.

---

**Provenance.** Issue: [mununu#548](https://github.com/Mumunu-team/mununu/issues/548) cause (b), from monono's ask 26. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
