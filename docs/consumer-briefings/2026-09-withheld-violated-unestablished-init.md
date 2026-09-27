# Consumer briefing — 2026-09 a VIOLATED that rested on an unestablished initial value is now withheld

> **Audience:** monono first — this came out of your [#577](https://github.com/Mumunu-team/mununu/issues/577) and it changes how you should read verdicts you already have. Also ROSF, and anyone verifying a design with an **asynchronous** reset.
>
> **Related:** contains [mununu#577](https://github.com/Mumunu-team/mununu/issues/577); does **not** close it. The cause is [mununu#578](https://github.com/Mumunu-team/mununu/issues/578); the missing self-check is [mununu#579](https://github.com/Mumunu-team/mununu/issues/579).
>
> **TL;DR:** **verdict-semantics change.** A `VIOLATED` on a **universal** property whose cone touches a register with no established cycle-0 value — which is what an async reset plus reset-gating produces — was **unsound**, and is now withheld as `unknown` with `bottom_reason.kind == "unestablished-initial-state"`. `HOLDS` is unaffected. Existential refutations are unaffected. **Re-read any VIOLATED you have on an async-reset design verified with a pinned reset.**

## ⚠️ Action item: which of your VIOLATEDs were in this class

The suspect set is narrow and mechanical:

- the property is **universal** — `AG`, a box modality, an SVA `assert property` that lifts to one; **and**
- the design's reset is **asynchronous** (`always_ff @(posedge clk or negedge rst_n)`); **and**
- the run **pinned the reset inactive** — the report says *"reset(s) pinned inactive"*, or you passed `--config-value <rst>=1`; **and**
- the verdict came back **VIOLATED**.

Those now return `unknown` instead. Anything outside that set is unchanged.

```bash
mununu --quiet sv verify-auto design.sv --json \
  | jq '[.properties[] | select(.bottom_reason.kind == "unestablished-initial-state")
         | {property: .name, registers: .bottom_reason.registers}]'
```

**A count here is not a new problem — it is an old wrong answer becoming visible.** Those properties were being reported as violations of your RTL. They were violations of a model that had lost your reset.

## What was happening

`sprite_eval.sv` resets asynchronously. yosys `async2sync` lowers an async reset into a **mux**, not a BTOR2 `init`. Measured on your lifted model, using the dump from [#552](https://github.com/Mumunu-team/mununu/issues/552) that shipped six days earlier:

```
init lines:  1
state lines: 11
```

Reset-gating then pins `rst_n=1` to keep the design out of reset — which **deletes the only remaining path to the reset values**, and nothing replaces it. Every affected register is free at cycle 0 and forever after.

`AG(drop_q <= 1)` is then **genuinely violated in that model**: `drop_q` may simply start at 1023. On RTL that bounds it at 1. The `AG(drop_q <= k)` sweep is what settled it — violated for every k from 1 to 1022, holding only at 1023, i.e. the engine believed the register reaches its 10-bit maximum.

A free start set **over-approximates** reachability, and that is sound for `HOLDS` and unsound for `VIOLATED`. This was that, exactly.

## ⚠️ CORRECTION (2026-09-27): your framing was right, and an earlier version of this briefing said otherwise

**An earlier version of this page claimed your `EF(drop_q == 2) VIOLATED` was "the correct half" and
that #577's framing was inverted. That was wrong, it was not measured, and it is retracted here.**

The reasoning was: a freer start set makes strictly more states reachable, so *"unreachable even
from this larger set"* is a **stronger** claim and an existential refutation stays sound. That
principle is true — but only for a refutation actually computed over that same freer model, and
that qualifier is the whole argument. I asserted your verdict satisfied it without checking.

Measured, on the free-init model itself (the #552 dump, with `drop_q == 2` turned into a `bad` node
and handed to the safety portfolio):

```
verdict: violated        <- the bad node IS reachable
reachable_by: [btormc]
```

`drop_q` is free at cycle 0, so `drop_q == 2` is reachable **immediately**. `EF(drop_q == 2)` is
therefore **true** on that model, and a `VIOLATED` there is not a sound refutation — it is simply
wrong. **So both halves of the pair were wrong, the report genuinely did contradict itself, and you
read it correctly.**

What that implies, stated as the hypothesis it is rather than as a second confident mechanism: the
two verdicts most likely came from **engines that disagree about the initial state**. From a
zero-init start, the design bounds `drop_q` to `{0,1}` and `drop_q == 2` really is unreachable — so
an engine assuming zero-init would report exactly the `EF` you saw, while an engine leaving
init-less registers free reports exactly the `AG` you saw. One report, two initial states. mununu's
own source already warns about that class of divergence for reset-less designs
(`reset_init::inject_zero_init`). We have not measured which engine decided which half of *your*
report, and will not claim it until we do; it is now the first thing
[mununu#579](https://github.com/Mumunu-team/mununu/issues/579) has to answer.

**None of this changes the fix or its validation** — the free registers, the `AG(drop_q <= k)`
sweep, and the 6/6 `HOLDS` after [mununu#578](https://github.com/Mumunu-team/mununu/issues/578) are
all measured and unaffected. What changes is the attribution of the `EF` half, and the lesson I drew
from it. The real lesson is the one #577 already contained: **the report was checked against the
design, and nothing checked it against itself** — including, it turns out, by me.

(The other half of the original note stands: `drop_q`'s actual bound is `{0,1}` — three writes, not
the two the issue's argument assumed, and your own SVA comment says so.)

## What you will see instead

```
[assert] sprite_eval_sva_sva_0: UNKNOWN/⊥ (0 cell(s))
      bottom-reason [unestablished-initial-state] determinism=reproducible: the property's cone
      touches 6 register(s) whose INITIAL VALUE was never established … a free cycle-0 state
      OVER-approximates reachability, and that licenses a definite HOLDS but never a definite
      VIOLATED … Establish the reset values (pin the reset ACTIVE for a cycle, or supply `init`
      via a sidecar) and re-run.
```

On the real design: three spurious VIOLATEDs became `⊥` with per-property counts of **5, 6 and 6** — each property's own cone, not a blanket downgrade — and the two genuinely-holding properties were untouched.

| field | value |
|---|---|
| `kind` | `unestablished-initial-state` |
| `registers` | init-less registers in **this property's** cone — a field, so you never parse `detail` for it |
| `determinism` | `reproducible` — a property of the model, not the host |
| `budget` / `budget_knob` | **absent.** No budget fired; raising one cannot help. A gate that retries on every ⊥ must not retry this one |

## The workaround, until #578 lands

**Establish the initial state rather than pinning the reset away.** Either hold the reset **active** for a cycle so the design enters its reset state, or supply `init` values via a sidecar. Both give the model the cycle-0 state that `async2sync` dropped.

[mununu#578](https://github.com/Mumunu-team/mununu/issues/578) is the repair: reset-gating should *establish* the reset values it pins away, by synthesising `init` from the reset mux — whose `ite(!rst_n, RESET_CONST, d)` shape mununu's FSM scanner already recognises. Its acceptance criterion is concrete and yours: those three `drop_q` bounds come back **HOLDS**.

**#577 stays open behind this.** Per our `Repro:` rule a containment may not close the cause — turning a wrong answer into an honest `⊥` is not the same as getting the right answer, and closing the issue would retire the investigation while the defect is still there.

## The other follow-up: nothing checked the report against itself

[mununu#579](https://github.com/Mumunu-team/mununu/issues/579). mununu has an `engine-contradiction` alarm, but it compares **engines**. This was **one engine, inconsistent across properties in one report**, and no check existed for that. A report contained its own refutation and shipped.

Deliberately, that check must **not pick a winner** — it must force both verdicts to `⊥`. Adjudicating would have picked the `AG`, which was the wrong one.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

A consumer pinning a mununu commit needs the new commit to see the change.

## Not covered here

- **Synchronous resets.** A sync reset lifts to a BTOR2 `init` and was never affected.
- **Getting the right answer.** This withholds a wrong one. #578 is what makes those properties decide.
- **A second bug fixed in the same change, invisible to you.** `ModelFacts::leaf_cells` was swallowing a parse refusal into an empty vector, so a design containing a **memory** measured as *"zero bits wide"* and the planner announced `cone 0b ≤ 40b cap → exact decides definitely` for a nine-register design. Unknown and zero are now distinguished. It affected only planner telemetry, never a verdict — but if you ever saw a `0b` cone in a plan note on an array-bearing design, that was this.

---

**Provenance.** Issue: [mununu#577](https://github.com/Mumunu-team/mununu/issues/577), with the diagnosis measured via [#552](https://github.com/Mumunu-team/mununu/issues/552)'s `MUNUNU_SHADOW_BTOR2_DUMP`. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
