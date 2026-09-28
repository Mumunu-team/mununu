# Consumer briefing — 2026-09 name your annotation properties, and a report that contradicts itself now says so

> **Audience:** monono — you asked for the first of these explicitly after mununu#544 landed, and the second came out of mununu#577. ROSF, if you pin annotation properties.
>
> **Related:** closes [mununu#550](https://github.com/Mumunu-team/mununu/issues/550) and [mununu#579](https://github.com/Mumunu-team/mununu/issues/579).
>
> **TL;DR:** additive, no verdict changes on any existing model. `@mununu_guarantee(<name>)` gives an annotation property a stable pin — **optional, never inferred**, exactly as you asked. Separately, a report whose own verdicts are mutually unsatisfiable now forces **both** to ⊥ with a soundness alarm. One correction included: mununu#579's own worked example turns out not to be a contradiction, and we are not flagging it.

## 1. `@mununu_guarantee(<name>)` — the half mununu#544 left open

Your words when you filed it:

> Our tier-3 pins are exactly the annotation-derived ones — `line_table`'s twin pins
> `ann_guarantee_0`, `affine_sampler`'s witness pins `ann_guarantee_1`. So the index-fragility ask is
> half closed, and the half still open is the half holding our recoverability tier.

```systemverilog
// @mununu_guarantee(tier3_recover) nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)
// @mununu_guarantee               nu X. ((drop_q <= 1) && [] X)
```

The name becomes the property's **label**, which `--expect` already matches (#544). The positional `ann_guarantee_<index>` is **kept**, so your existing index pins keep working — you can name the ones that matter and leave the rest.

**Optional, never inferred**, which was your constraint and the right one. An unnamed annotation stays unnamed. Nothing is derived from the formula text, the source line, or the enclosing module: a name that moves when you reformat a formula would be the index-fragility again in a different costume.

### Why the name is on the tag and not in the body

We considered a `name:` prefix inside the value and rejected it on a concrete collision: since [#565](https://github.com/Mumunu-team/mununu/issues/565) admitted bit-slice atoms, a body can legitimately contain a colon — `nu X. ((wdata[7:0] == 165) && [] X)`. Any split rule over the value has a real ambiguity there; the parentheses have none. There is a test pinning exactly that case.

A name may contain `[A-Za-z0-9_.-]`. A malformed group — empty, containing a space, unclosed — leaves the tag unrecognised and the annotation is **skipped with a message**. Worth knowing because the old behaviour for `@mununu_guarantee(foo)` was to drop the annotation **silently**: the whole `guarantee(foo)` token failed tag lookup and nothing was said. If you ever tried the parenthesised form before today, it did nothing and told you nothing.

### A duplicate name is refused

```
@mununu_guarantee(dup) — duplicate name, already used by `ann_guarantee_0`. A name is a PIN:
two properties answering to one would make `--expect dup` silently match only the first.
Rename one of them.
```

**This is deliberately stricter than SVA labels**, and the asymmetry has a reason rather than being an oversight: two asserts inside one labelled `begin…end` legally inherit the same label, so refusing duplicates there would reject valid SystemVerilog. An annotation name is author-chosen, one per annotation, and has no such excuse.

### Scope note

The **attribute** form (`(* mununu_guarantee = "…" *)`) does not carry a name. Measured across your `rtl/` tree: **180 `// @mununu_guarantee`, 8 `// @mununu_predicate`, and zero attribute-form annotations.** Shipping a second naming mechanism for a syntax with no users would be surface with no reader; if you start using it, say so and it gets one.

## 2. A report that contradicts itself (mununu#579)

`engine-contradiction` already catches two *engines* returning opposite definite verdicts on one property. It could not catch one engine returning verdicts that are mutually unsatisfiable across *different properties* — which is how #577 reached you. Nothing looked, and your report was the detector.

A new `bottom_reason.kind`:

| field | value |
|---|---|
| `kind` | `report-self-contradiction` |
| `detail` | names **both** properties and that their atoms are exactly negated |
| `determinism` | `reproducible` |

**Both properties are forced to ⊥. It does not pick a winner** — and that is the design, not a limitation. #577 taught it expensively: the natural reading there (*"the engine is right at 0 and 1 and wrong at the boundary"*) blamed the half that was correct. A contradiction establishes that one verdict is broken, not which one. **Retrying is actively wrong, and so is believing whichever looks more plausible.** Escalate it.

What is admitted is deliberately narrow — only `AG(P)` VIOLATED beside `EF(¬P)` VIOLATED over **exactly negated** single-comparison atoms on the same register:

```
AG(P) VIOLATED  =>  some reachable state satisfies ¬P
EF(Q) VIOLATED  =>  no reachable state satisfies Q      (with Q ≡ ¬P: unsatisfiable together)
```

### ⚠️ Correction: #579's own example is not a contradiction

The issue proposed a broader rule — *"for `AG(x <= K)` VIOLATED, at least one `EF(x == v)` with `v > K` must not be VIOLATED"* — and asserted of its own example that the verdicts "cannot all hold". **They can**, and we are not implementing it:

```
AG(drop_q <= 1)  VIOLATED   =>  some reachable state has drop_q > 1
EF(drop_q == 2)  VIOLATED   =>  2 is never reached
EF(drop_q == 3)  VIOLATED   =>  3 is never reached
```

A model reaching **7** and never 2 or 3 satisfies all three. Two witnesses pin two values out of the 1022 that `> 1` admits on a 10-bit register, so they refute nothing; the rule only becomes sound once the witnesses **exhaust** the range — 1022 properties here — which no real report carries.

So **#577's pair is outside the sound fragment, and that is the finding rather than a gap.** What was wrong there was a *verdict*, not a pair of verdicts: no report-internal check could have caught it, because the report was internally consistent. The detector for that class is the init-convention audit below, not this pass.

Implementing the broader rule would force two verdicts to ⊥ on evidence that permits both. A false soundness alarm destroys sound results, which is a worse failure than the one it would be closing — so the omission is deliberate and has a test recording it.

## 3. What the audit found: three paths disagree about cycle 0

#579's first question was whether the engines even start from the same state. They do not:

| path | init-less state cell | direction |
|---|---|---|
| predicate cube — `state_cell_init_values` | **0** | fewer start states ⇒ under-approx, unsound for `HOLDS` |
| exact — `initial_state_bdd` | **free**, since [#498](https://github.com/Mumunu-team/mununu/issues/498) | more start states ⇒ unsound for `VIOLATED` |
| reachability portfolio (native BMC / SPACER / btormc / Pono) | **free** (BTOR2 semantics) | unsound for `VIOLATED` |

Opposite soundness postures on cycle 0 inside one portfolio run. **That is the leading explanation for #577's pair of mutually-wrong verdicts** — from zero-init the design bounds `drop_q` to `{0,1}` and `== 2` really is unreachable; with init-less registers free, `drop_q` can start at 1023.

Three comments in our own source claimed both engines defaulted to 0 — stale since #498, and one of them was cited as evidence during #577's diagnosis. They now carry the table above, and a test pins the cube's behaviour so the next drift fails a build instead of a consumer's gate.

**Converging the three is a behaviour change that needs its own measurement**, so it stays open on #579 rather than riding along here. After [#578](https://github.com/Mumunu-team/mununu/issues/578) the window is small — a cell only reaches the divergence if it has no recoverable reset value, i.e. a `--cutpoint` or a flop that holds through reset.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **The attribute form's name.** See the scope note above — zero users measured in your tree.
- **Converging the init conventions.** Open on #579, with the table recorded in `reset_init.rs`.
- **The broader bound/witness rule.** Refuted above; not implemented, with a test recording why.

---

**Provenance.** Issues: [mununu#550](https://github.com/Mumunu-team/mununu/issues/550) (requested by monono after #544), [mununu#579](https://github.com/Mumunu-team/mununu/issues/579) (filed off #577). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
