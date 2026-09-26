# Consumer briefing — 2026-09 multi-label transitions are synchronisation vectors, and a dead one now warns

> **Audience:** anyone composing CTXDSL automata — monono's fabric and VPU models especially. If you have ever written `transition s -> t on label a, label b;` meaning *"either may happen"*, read this.
>
> **Related:** closes [mununu#570](https://github.com/Mumunu-team/mununu/issues/570).
>
> **TL;DR:** no semantics changed. **The documentation was wrong**, and there is now a warning for the case it misled people into. `on label a, label b` is **one compound action `{a,b}`** — it does not fire on `a`, and it does not fire on `b`. `docs/abstraction.md` claimed the opposite ("collapse parallel edges", soundness "Exact"); both halves were false, and the cost is a composition that silently freezes.

## ⚠️ Action item: check your "I ignore the other labels" self-loops

This is the shape that bites:

```
// WRONG — ONE unsatisfiable compound action. This automaton freezes under composition.
transition Live -> Live on label ld_write, label ld_done, label cpu_write, label retire;

// RIGHT — four alternatives, any one of which may fire.
transition Live -> Live on label ld_write;
transition Live -> Live on label ld_done;
transition Live -> Live on label cpu_write;
transition Live -> Live on label retire;
```

A real 7-automaton model carried 19 self-loops in the collapsed form. Every one became an
unsatisfiable compound action, the composition had **one reachable state**, and all three
formulas returned confident verdicts — **including a safety property that "held" vacuously**.
Expanding them into single-label transitions brought the model back and flipped every verdict.

**Re-run any composed model you have verified since you started using the collapsed form.** A
passing safety property over a frozen composition is the failure mode here, and it looks exactly
like a passing safety property.

## The rule, stated once

Two transitions pair only when their projections onto the **shared alphabet** are equal as sets.
The shared alphabet is the intersection of the two automata's alphabets.

| partner's transitions | shared alphabet | `Multi` (`on a, b`) projects to | partner projects to | fires? |
|---|---|---|---|---|
| `U0->U0 on a;` **and** `U0->U0 on b;` | `{a,b}` | `{a,b}` | `{a}` / `{b}` | **no** |
| `U0->U0 on b;` only | `{b}` | `{b}` | `{b}` | yes |
| `U0->U0 on a, label b;` | `{a,b}` | `{a,b}` | `{a,b}` | yes |

**Row 1 is not monotone, and that surprises everyone.** Its partner permits strictly *more*
behaviour than row 2's, yet the composition permits strictly *less*. Adding `a` to the partner
does not merely grant a capability — it **enlarges the shared alphabet**, which retroactively
changes `Multi`'s own projection from `{b}` to `{a,b}`. The alphabet is structural, not a
permission set. This is ordinary alphabetised-parallel behaviour in the CSP family; it is only
surprising because a comma reads like "or".

## The new warning

Emitted at composition time, before any state-space work:

```
[mununu#570] compound action {ld_write, ld_done, cpu_write} can never fire: no transition
in the partner automaton projects to the same set over the shared alphabet {…}. A multi-label
transition is a SYNCHRONISATION VECTOR — it fires on the whole set together, not on any one
label. If these are independent alternatives, write them as separate single-label transitions.
```

**It catches the partial case, which is the dangerous one.** The existing warning —
`model has 1 reachable state(s); verdicts are vacuously satisfied` — only fires on a *total*
collapse. A model where *some* compound edges are dead merely shrinks, the verdicts are still
wrong, and nothing said so. This one fires per dead action regardless.

Two passes over the transition lists, no product construction, so it costs nothing measurable.

**It does not fire** when a compound's shared projection is empty — those labels are local to that
automaton, so it interleaves freely and is fine. That exclusion is deliberate: a false positive
would train you to ignore the warning.

## When the collapsed form is CORRECT

It is not a deprecated spelling. A synchronisation vector is the right primitive whenever the
source feature is genuinely one atomic joint action:

- a turn-based round carrying both an environment and a controller action — `emit.rs`'s
  `{env_*, ctrl_*}` pairs, which the Skolem/Mealy game encoding and GR(1) synthesis are built on;
- a handshake where two parties must move together — `examples/verify/v10_mem_fabric_client_mux`,
  `on label grant, label refuse`.

Both mean *and*, and both would be silently wrong as parallel edges. **This is why the semantics
was not changed to match the documentation** — doing so would have corrupted models that are
correct today.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **A distinguishing syntax.** `on a \| b` for alternation vs `on {a, b}` for a sync vector would
  make the intent unambiguous at the point of writing, rather than at the point of warning. That
  is the real fix and it is a grammar change; this is not it.
- **No verdict changes.** Composition behaves exactly as before. What changed is the
  documentation, `CLAUDE.md`'s anti-pattern (which previously pushed authors *into* the collapsed
  form and now warns in both directions), and the new diagnostic.

---

**Provenance.** Issue: [mununu#570](https://github.com/Mumunu-team/mununu/issues/570), filed with a
four-row repro that is reproduced verbatim in
[`docs/abstraction.md`](../abstraction.md). Policy:
[`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
