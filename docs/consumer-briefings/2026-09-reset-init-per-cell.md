# Consumer briefing — 2026-09 a memory's `init` no longer leaves every flop free at cycle 0

> **Audience:** monono first — this is the cause behind [#577](https://github.com/Mumunu-team/mununu/issues/577) and it turns the ⊥s from the previous briefing back into verdicts. Anyone verifying a design that contains **both a memory and async-reset registers** is affected, which is most RTL.
>
> **Related:** closes [mununu#578](https://github.com/Mumunu-team/mununu/issues/578), and with it [mununu#577](https://github.com/Mumunu-team/mununu/issues/577). Read [`2026-09-withheld-violated-unestablished-init.md`](2026-09-withheld-violated-unestablished-init.md) first if you have not.
>
> **TL;DR:** **verdict-semantics change, in the direction you want.** Registers that were silently free at cycle 0 now start at their real reset values. On `sprite_eval`, measured: **6/6 HOLDS, 0 ⊥** — the three properties that were spuriously `VIOLATED`, then withheld as `⊥`, now decide. The faulty twin still violates exactly the two it must.

## What was actually wrong

mununu has established post-reset initial state since well before #577. `inject_reset_init` simulates one reset-asserted cycle before the reset pin is applied and writes an `init` line per register — exactly the "hold reset, then release" semantics.

It has a guard, and the guard was the bug:

```rust
// before
if file.lines.iter().any(|l| matches!(&l.node, Node::Init { .. })) {
    return Ok(content.to_string());
}
```

The intent is sound — *a design with an authoritative BTOR2 `init` must not be advanced past*. The **scope** was not. It is per-**design**, and yosys emits an `init` for **every memory it lifts**. So one array init — a line that says nothing whatever about the scalar flops — switched the entire pass off.

On `sprite_eval`, measured:

```
init lines:  1     <- the memory
state lines: 11    <- ten async-reset flops, all left free
```

Those free registers are what produced `AG(drop_q <= 1) VIOLATED` on a counter the RTL bounds at 1.

**So the mechanism was never missing. It was disabled by an unrelated line, on every design with a memory in it** — which is why this looked like a deep soundness problem and is a three-line scope fix.

## What changed

Eligibility is now decided **per state cell**, matching what the sibling `inject_zero_init` already did:

| cell | before | now |
|---|---|---|
| already has `init` | untouched | untouched — authoritative |
| async-reset flop, no `init` | **left free** if the design had any init anywhere | **starts at its reset value** |
| array / memory | — | skipped: a `constd` init is ill-typed at an array sort |
| no `next` (a `--cutpoint`, a blackboxed output) | — | **left free**, deliberately |

**The last row is not tidiness.** A cutpoint is free at *every* cycle because you asked for it. Pinning its cycle-0 value removes start states, which — unlike #577 — would be unsound for **HOLDS**, the more dangerous direction. Fixing one direction must not open the other, so it is asserted by its own test rather than left to fall out.

## Measured on `sprite_eval`, with your own pins and cutpoint

```
[assert] sprite_eval_sva_sva_0: HOLDS      <- was VIOLATED, then ⊥
[assert] sprite_eval_sva_sva_1: HOLDS      <- was VIOLATED, then ⊥
[assert] sprite_eval_sva_sva_2: HOLDS      <- was VIOLATED, then ⊥   (AG(drop_q <= 1))
[assert] sprite_eval_sva_sva_3: HOLDS
[assert] ann_guarantee_0:       HOLDS
[assert] ann_guarantee_1:       HOLDS
6 assertion(s): 6 definite (HOLDS), 0 violated, 0 unknown (⊥), 0 skipped
```

**And the control, which is the half that makes the above mean anything.** Your faulty twin — the
budget check removed, same pins, no cutpoint — was run unchanged:

```
[assert] sprite_eval_faulty_sva_sva_0: VIOLATED (1 cell)
[assert] ann_guarantee_0:              VIOLATED (1 cell)
```

Exactly the two `verify.sh` requires. A fix that established the reset by over-constraining the
model would have made these hold too; rule 5 is what distinguishes the two outcomes, and it is why
the twin was run rather than assumed.

Your `--cutpoint vrecip` is untouched — a cutpoint has no `next`, so this pass deliberately leaves
it free.

## Re-run everything

This changes the **initial state** of any design with a memory, which is the most load-bearing thing a model has. Expect movement in both directions and treat neither as automatically right:

- `unknown` → `holds` / `violated`: the intended effect.
- `violated` → `holds`: a previously spurious violation, the #577 class.
- `holds` → `violated`: **read this one carefully.** A property that held only because a register could start anywhere was holding for the wrong reason, or vacuously. It is now being checked from the real reset state.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **Registers with no reset.** A flop that holds through reset (`next = ite(rst, d, q)`) genuinely has no reset value; it stays free, and the #577 guard still withholds a `VIOLATED` whose cone touches one. Correct, not a gap.
- **Cut points do not trigger that guard.** A cut lifts to a state with no `next`, and the #577 guard now ignores those. Counting them withheld a *sound* `VIOLATED` — `AG(st_q == 0)` over a design whose `st_q` leaves 0 regardless of the cut register — which is a precision loss, and it broke the contract that applying a cut may not change a verdict. Caught by `e2e_cutpoint_stays_an_over_approximation_no_verdict_flips`, which is exactly what that test is for.
- **Memory contents.** A memory's own `init` is authoritative and untouched.
- **Simulation vs. the mux read.** mununu derives the post-reset state by simulating one
  reset-asserted cycle. That simulator does not implement the array operators, so a design that
  **reads** a memory cannot be simulated — the coarse guard had been hiding that too. Such a design
  now falls back to reading each reset value directly off the reset mux's dead arm. The fallback
  reads strictly less (it cannot evaluate a *computed* reset, and does not advance a reset-less
  register by a cycle); a register it cannot read stays free rather than being guessed at zero.

---

**Provenance.** Issue: [mununu#578](https://github.com/Mumunu-team/mununu/issues/578), the cause
behind [mununu#577](https://github.com/Mumunu-team/mununu/issues/577). Validated end-to-end in the
`mununu-sva` image against `rtl/vpu/sprite_eval` and its faulty twin. Policy:
[`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
