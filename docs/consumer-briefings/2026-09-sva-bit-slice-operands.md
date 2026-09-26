# Consumer briefing — 2026-09 a bit-slice may now be a comparison operand

> **Audience:** monono, anyone writing SVA that mununu verifies. Card B-23 (`rtl/vpu/vpu_regs`) is the one that paid for this.
>
> **Related:** closes [mununu#565](https://github.com/Mumunu-team/mununu/issues/565).
>
> **TL;DR:** a bit-slice is now a legal comparison operand — **nothing is refused for being a slice any more.** `wdata[7:0] == 8'hA5` translates *and decides*. `q == $past(src[7:0])` translates but currently **skips** on a pre-existing rule about relational atoms — read "the boundary" below before assuming your two residual instances are unblocked. **Verdict-semantics change:** properties that came back in `unsupported` now produce verdicts or a named skip, so a lane asserting "N unsupported" will see N shrink.

## What changed

Previously a comparison whose operand was a bit-slice was refused:

```
[unsupported] slice_sva_sva_0: comparison must be `signal == literal`, … ;
got left=Some("RangeSelect") right=Some("IntegerLiteral")
```

while the same claim over the whole word was accepted and decided. The limitation was the slice,
not the comparison. Both forms now **translate**; one of them also decides:

```systemverilog
a_slice_vs_literal: assert property (@(posedge clk) disable iff (!rst_n)
    (we && (wdata[7:0] == 8'hA5)) |=> (shadow_q == 8'hA5));   // was refused — now HOLDS

a_past_slice:       assert property (@(posedge clk) disable iff (!rst_n)
    we |=> (shadow_q == $past(wdata[7:0])));                  // was refused — now SKIPS,
                                                              // with a named reason (see below)
```

## What you will see in the output

A slice becomes a **named derived signal** in the lifted model, so the atom in a formula, a
counterexample or a `seeded_predicates` list is a plain identifier:

| SVA | atom |
|---|---|
| `wdata[7:0]` | `wdata__bits7_0` |
| `$past(wdata[7:0])` | `wdata__past__bits7_0` |

That naming is stable and part of the contract. It is not cosmetic: `[` already means a box
modality to the mu-calculus parser and an array index to the predicate layer, so a bracketed atom
would **misparse** rather than fail. Minting a derived signal — exactly how a `$past` base becomes
a shadow register — keeps every layer below the translator untouched.

`$past` over a slice slices the **shadow**, i.e. the one-cycle-old whole signal. No new state is
added for the slice itself; it is combinational.

## ⚠️ The boundary: a slice inside a RELATIONAL atom still skips

Measured end-to-end, not predicted:

| property | outcome |
|---|---|
| `(we && wdata[7:0] == 8'hA5) \|=> shadow_q == 8'hA5` | **holds** |
| `we \|=> shadow_q == $past(wdata[7:0])` | **skipped**, with a named reason |
| `(we && wdata == 32'h…) \|=> shadow_q == 8'hA5` (control) | holds |

The skip reads:

```
Skipped { reason: "atom(s) over non-state signals (combinational/IO — not cube-bindable):
                   shadow_q == wdata__past__bits7_0" }
```

**Why, and why it is not new.** A *simple* atom (`signal == literal`) may be a state cell, a free
input, **or an input-dependent combinational** — all three seed. A *relational* atom
(`reg == reg`) requires its registers to be **all-state**; that rule predates this change. A slice
is combinational by construction, so slicing `wdata__past` takes the compound out of the all-state
class. `shadow_q == $past(wdata)` — unsliced, both sides state — seeds fine.

**What this means for your two residual `$past(wdata[7:0])` instances: they will translate and
then skip, not decide.** They are no longer *refused*, and the skip names the atom and the reason
rather than blaming the operand kind — but if you were waiting to delete the workaround, wait.
Lifting the all-state rule for relational atoms is separate work and not in this change.

## ⚠️ Indexed part-selects are still refused, on purpose

```systemverilog
wdata[7 -: 8] == 8'hA5    // refused, with a reason
```

Measured against slang 11.0.448: `wdata[7 -: 8]` serialises as `selectionKind: "IndexedDown"` with
**`left: 7, right: 8`** — that is the base index and the **width**, not a high and a low bit.
Reading those two numbers as a range gives bits `[7:8]`: inverted, wrong, and with no error
anywhere.

So the translator declines and says why, rather than mis-slicing silently. **Rewrite as
`wdata[7:0]`.** Widening to the indexed forms needs its own arithmetic and its own tests; it is a
follow-up, not a missing arm.

## Action item: your unsupported counts will drop

This is a **verdict-semantics change** in the direction you want, but it is still a change:

- a property that was `unsupported` may now be `holds`, `violated`, or `unknown`;
- any expectation pinned with `--expect-unsupported` or a count assertion needs re-baselining;
- `AllHold` lanes that previously passed *because* a slice property was excluded will now actually
  check it.

Re-run and re-baseline before reading a new red as a regression.

**Your two residual `$past(wdata[7:0])` instances will move from `unsupported` to `skipped`, not
to a verdict** — see the boundary section above. Keep the workaround for those two.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none — uses the slang already pinned there | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **Indexed part-selects** (`[i +: w]` / `[i -: w]`) — refused, see above.
- **A slice of anything but a plain signal** — a slice of a slice, or of a concatenation — is
  refused for the same reason: only `signal[hi:lo]` is in scope.
- **Bit-selects** (`wdata[3]`) remain refused by the pre-existing `ElementSelect` branch; this
  change did not touch it.
- The accepted **operand** fragment is now documented in
  [`docs/verifying-rtl.md`](../verifying-rtl.md) §3. It previously existed only inside the refusal
  message, which is why this cost two rewrites rather than one lookup — that gap is the other half
  of what shipped here.

---

**Provenance.** Issue: [mununu#565](https://github.com/Mumunu-team/mununu/issues/565). Policy:
[`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
