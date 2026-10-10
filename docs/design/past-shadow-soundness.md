# `$past` shadow registers — the soundness argument, and where it stops

> Status: shipped, with one stated approximation and two pre-existing defects
> named in §6. This doc exists because `augment_with_past_shadows` was extended to
> accept **primary inputs** as shadow bases (previously state cells only), and
> "the flop is real, so the verdict transfers" was asserted in a commit message
> without being written down anywhere. The construction turns out to be sound for
> a stronger reason than that sentence gives — and to have a boundary the sentence
> hides.

## Contents

1. [The construction](#1-the-construction)
2. [Why it is sound: history variables](#2-why-it-is-sound-history-variables)
3. [The initial state — the one place the argument stops](#3-the-initial-state--the-one-place-the-argument-stops)
4. [Why the input shadow is pinned rather than left free](#4-why-the-input-shadow-is-pinned-rather-than-left-free)
5. [Interaction with the KMTS fixpoint](#5-interaction-with-the-kmts-fixpoint)
6. [Two defects this surfaced](#6-two-defects-this-surfaced)
7. [Stutter equivalence — why nothing here may ever assume it](#7-stutter-equivalence--why-nothing-here-may-ever-assume-it)
8. [Per-engine ledger](#8-per-engine-ledger)
9. [References](#9-references)

---

## 1. The construction

The SVA translator cannot express `$past(b)` as an atom — there is no signal in
the model holding "b, one cycle ago". So the BTOR2 gains one:

```
n     state <sort> b__past
n+1   next  <sort> n <b>
n+2   init  <sort> n <v>        ; v = b's own init (state base) or zero (input base)
```

`$past(b, k)` (k ≥ 2) gains a **k-stage shift chain** instead — `b__past ← b`,
`b__past2 ← b__past`, … `b__past{k}` — so `b__past{j}` holds b's value j cycles
ago. Each stage is init'd the same way as the 1-stage case (below). Everything
that follows is written for one stage; each stage of a chain is an independent
history variable and the argument composes over them.

`b` is either a BTOR2 state cell or a primary input;
[`resolve_shadow_source`](../../crates/mununu-core/src/adapter/btor2/shadow.rs)
tries a unique exact state symbol, then an exact input symbol, then the
symbol-distance cone walk. The atom `$past(b)` then translates to the ordinary
signal reference `b__past`.

This is the same move [`free-input-atoms.md`](free-input-atoms.md) is about, in
its temporal form: convert a reference the abstraction cannot bind into a state
reference it can.

## 2. Why it is sound: history variables

`b__past` is a **history variable** in the Abadi–Lamport sense: its next-state
value is a function of the current state and inputs, and nothing in the design
reads it. Two consequences, and they are the whole argument.

**It adds no behaviour.** Let `M` be the original system over variables `V` and
`M'` the augmented one over `V ∪ {b__past}`. The projection `π : S' → S` that
forgets `b__past` is a *functional bisimulation*: every `M'` transition projects
to an `M` transition, and every `M` transition lifts to exactly one `M'`
transition per source state. So for any formula `φ` over `V` alone,

> `M', s' ⊨ φ  ⟺  M, π(s') ⊨ φ`

The augmentation is a **conservative extension**. It cannot create or destroy a
counterexample to any property that does not mention the shadow.

**It removes no behaviour.** `next(b__past) = b` is total — it constrains only the
new variable, never `b`. This is the specific reason a *prophecy* variable would
need a different argument and a history variable does not: prophecy variables
guess the future and can prune traces; history variables record the past and
cannot.

Nothing above distinguishes a register base from an input base. `next` sourced
from an `input` node is well-formed BTOR2 with exactly the intended meaning — the
flop samples the input at cycle `t` and presents it at `t+1`. The old restriction
to state cells was conservative, not semantic.

## 3. The initial state — the one place the argument stops

A history variable has no defined value before the first transition, and §2 says
nothing about it. Two cases:

- **State base.** The shadow mirrors the source's `init`, so `b__past == b` at
  cycle 0 — the "no history before the first clock edge" convention. If the
  source has no `init`, neither does the shadow, and the pair moves together
  under whatever a given engine does with an init-less cell.
- **Input base.** There is no `init` to mirror, so a value is *chosen*. That
  choice is an approximation, and §4 explains why it is a choice at all.

**When is the chosen value observable?** Exactly when a property reads the shadow
in the initial state — and the lift settles that structurally. `nu X. (body && [] X)`
evaluates `body` at every reachable state *including the initial one*, and the two
implication forms place the consequent differently:

| SVA | lifted formula | reads the invented value? |
|---|---|---|
| `push \|-> d0_q == $past(din)` | `nu X. ((!push \|\|    (d0_q == din__past)) && [] X)` | **yes** — atom outside `[]` |
| `push \|=> d0_q == $past(din)` | `nu X. ((!push \|\| [] (d0_q == din__past)) && [] X)` | no — atom under `[]` |

Under `[]` the atom is only ever read at a state that *has* a predecessor, where
the shadow holds a value the design actually drove. So the approximation is
invisible to every `|=>` shape — which is every data-integrity property, because
"sample an antecedent, check the result one cycle later" is what `|=>` means — and
visible to `|->`.

Pinned by `only_a_same_cycle_past_reads_the_invented_history` in
[`tests/past_shadow_input_e2e.rs`](../../crates/mununu-core/tests/past_shadow_input_e2e.rs),
so a future change to the lift that moved the atom out from under `[]` would fail
a test rather than silently widen the approximation.

**Depth k generalises the window, not the argument.** A depth-1 shadow is invented
only at cycle 0, so the single `[]` of `|=>` hides it completely. A depth-k chain
takes k cycles to fill, so `$past(input, k)` reads the invented zero for the first
k-1 post-reset cycles *even under a `[]`* — the approximation is bounded to those
cycles rather than eliminated. For a **register** base this is not an approximation
at all: those cycles read the source's reset value, which is exactly the SVA
"before the first k clock edges" convention. So k-deep `$past` of a *register* is
exact everywhere; k-deep `$past` of an *input* carries a k-1-cycle post-reset
window where a definite verdict may not transfer (depth 1 = the cycle-0 case above).

## 4. Why the input shadow is pinned rather than left free

Leaving it free is the more faithful reading of SVA, where `$past` is undefined
before the first clock edge. It is not a choice this adapter can make.

An init-less BTOR2 state cell means **different things to different engines**. The
cube and exact engines default it to 0 (`state_cell_init_values` /
`initial_state_bdd`, per the `setundef -zero` power-up); the reachability
portfolio leaves it FREE, per BTOR2's nondeterministic-init semantics. That is
precisely the verdict disagreement
[`reset_init::inject_zero_init`](../../crates/mununu-core/src/adapter/btor2/reset_init.rs)
exists to close — and it cannot close this one, because it runs on the
pre-augmentation BTOR2, before the shadow is appended. A free shadow escapes the
mitigation by construction.

So the shadow carries an explicit `init 0`, and every engine decides the same
model. What is given up is bounded by §3 and stated at the code.

> The `init` value's NID must be **below** the state's: btormc's parser requires
> it. mununu's own parser is order-agnostic, so the wrong order reads fine
> in-process and is rejected by the external oracle — a portfolio failure, not a
> parse error. The zero is therefore allocated before the flop.

## 5. Interaction with the KMTS fixpoint

The lift abstracts the augmented BTOR2 into predicate cubes with a may relation
(over-approximating the concrete transitions) and a must relation
(under-approximating them), and evaluates the μ-calculus 3-valued in the sense of
Bruns–Godefroid. Two things matter for the shadow.

**The shadow is what makes the atom bindable at all.** A cube is a valuation over
predicates on *state* variables. `$past(b)` names no state, so before the
augmentation the property translated to a formula over a name absent from the
model and every engine reported `skipped — predicate references unknown
register/signal`. After it, `b__past` is an ordinary state variable and predicates
over it partition cubes like any other. That is the entire point of the trick.

**The 3-valued modalities depend on `R_must ⊆ R_may`.** From
[`symbolic.rs`](../../crates/mununu-core/src/mu_calculus/symbolic.rs):

```text
[]φ.must = ∀ R_may.  φ.must        <>φ.must = ∃ R_must. φ.must
[]φ.may  = ∀ R_must. φ.may         <>φ.may  = ∃ R_may.  φ.may
```

The internal invariant every node must satisfy is `must ⊑ may`. For the box that
reduces to: *if all may-successors definitely satisfy φ, then all must-successors
possibly satisfy φ* — which holds given `φ.must ⊑ φ.may` **if and only if
`R_must ⊆ R_may`**. That containment is an obligation on the lift, not a theorem
about it; §6 is what happens when it is not met.

## 6. Two defects this surfaced

> **Update (2026-08-28) — fixed at the lift.** The root cause below (a must-edge
> created without enforcing `R_must ⊆ R_may`) is now closed in
> [`kmts_lift.rs`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) by three
> composing changes: (A) the `SmtAllPairs` standard-must arm restricts the ∀∃
> must-image to the emitted `may_edges` (`must_edges_over`) instead of the full
> grid, so a vacuous must out of an empty src — which the ∃-witness may-check never
> emits — can no longer be promoted; (B) `apply_sampled_must_inference` proves each
> candidate source cube feasible (`smt_source_cube_proven_feasible`, `∃ s. ⋀ src_i`)
> and drops must-promotion out of the proven-empty ones, closing the sampling path
> where the canonical representative fabricates a may-edge for an unsatisfiable cube;
> (C) `assert_must_subset_may` runs once per lift (release included) and returns an
> `IrConsistencyError` if any `MustHyperOnly` target is not a may-successor, rather
> than letting an inconsistent KMTS reach `TritBdd::from_parts`. Unit-tested in
> `kmts_lift.rs` (`must_subset_may_no_vacuous_must_from_unsat_cube_{all_pairs,sampling}`,
> `assert_must_subset_may_{rejects,accepts}_*`). The `from_parts` `debug_assert!`
> stays as the debug tripwire (§5); the release guarantee is now (C).
>
> **Update (2026-08-28, part 2) — the symbolic engine had a SEPARATE path.**
> (A)–(C) fix the cube / `kmts_lift` must-edge inference (the explicit + cube
> engines). The `symbolic` engine builds its **own** may/must abstraction in
> [`symbolic_bitblast.rs`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs)
> (`AbstractRelation`), which (A)–(C) do not touch — and an **e2e run in the
> `mununu-sva` image caught it still panicking** on a `$past` data-integrity
> property (`push |=> stored == $past(din)`). Root cause: an **input predicate**
> (`push`) makes the abstraction depend on an input var, which is both a present
> *observable* and the transition *nondeterminism* — the same conflation the exact
> ROBDD engine refuses an input atom over. It left `R_must`'s BDD support dirty, so
> `box.must ⊄ box.may`. Fix (D): `AbstractRelation::build` now detects an
> input-predicate abstraction (quantifying inputs changes feasibility) and falls
> back to **may-only** (`r_must = None`) — sound (definite HOLDS transfers;
> refutation → ⊥), no panic, no wrong verdict. `feasible_present` also now
> quantifies inputs (`xi_cube`), a no-op for register-only predicates.
>
> The table below is the **pre-fix** measurement. The `$past` e2e was run in the
> `mununu-sva` image (that is what caught (D)); a register-`$past` / `##k` design
> now decides cleanly (see the XL.4/XL.5 e2e). A `push |=> stored == $past(din)`
> input-antecedent property is may-only (⊥ under abstraction, or decided by the
> reach portfolio) — sound, never a spurious `Violated`/panic.

Both are **pre-existing** and reproduce identically on a *register*-sourced
`$past`, so neither comes from input support. A property with no `$past` is
unaffected in both postures. All three designs below are correct.

| posture | no `$past` | `$past` of a register | `$past` of an input |
|---|---|---|---|
| default (may-only) | Holds | Holds | Holds |
| `symbolic_engine: true` | Unknown | **panic** | **panic** |
| `must_edge_inference: SmtPerTarget` | Holds | **Violated** (false) | **Violated** (false) |

They look like one root cause. `kmts_lift.rs`'s must-edge post-pass iterates
`sampled_targets_per_source` and adds a `Sharp` transition for every pair the SMT
check accepts — **without checking that the pair is in the may relation.** Nothing
in the loop enforces `R_must ⊆ R_may`. When it is violated:

- the symbolic engine trips `TritBdd::from_parts`'s `debug_assert!` — which is
  compiled out in release, so the release failure mode is an inconsistent
  `TritBdd` and a **wrong verdict**, not a crash;
- the explicit engine resolves ⊥ cells against a must-path that does not exist
  concretely, and reports `Violated` for a correct design.

Until that is fixed, `must_edge_inference: Off` (the default) is the only sound
posture for a `$past` property. A definite HOLDS transfers — may over-approximation
plus a safety property. Refuting a *data-dependent* violation needs must-edges, so
today such a violation lands on ⊥ rather than VIOLATED; widening the data to one
bit, raising `max_iterations`, and predicate hints on both the source and the
shadow were each measured and all stay at ⊥.

## 7. Stutter equivalence — why nothing here may ever assume it

`$past` counts clock edges, and `|=>` is a *next-step* operator. The `[]` and `<>`
above are next-step modalities, and next-step μ-calculus is **not**
stutter-invariant: a transformation that preserves only stutter equivalence —
partial-order reduction, self-loop collapsing, step compression, any
"finite-stuttering" quotient — silently changes the meaning of every `$past`
property while preserving ordinary invariants.

mununu contains no such transformation. `grep -ri stutter` over the crate and the
docs returns nothing, and the lift is a step-exact predicate image: one abstract
transition per concrete one. So every engine preserves step count and the shadow
is safe under all of them.

This section exists because that fact is **invisible** — it is a property of what
the codebase does *not* do. The constraint it records is on future work: any
step-collapsing optimisation must exclude models carrying `__past` shadows, or
`$past` properties become quietly wrong rather than loudly broken.

## 8. Per-engine ledger

| engine | status on a `$past` model |
|---|---|
| reach portfolio (btormc / pono / Boolector) | **Exact.** Consumes the BTOR2 directly; the shadow is a real flop. Requires the `init` value's NID to precede the state's — see §4. |
| exact-symbolic (ROBDD, 2-valued) | **Exact** on the augmented model. A property whose atom names a primary input (`push \|=> …`), or a signal combinationally driven by one, is since mununu#656 evaluated over the product of the registers with that input sampled in the same cycle (`adapter/btor2/input_state.rs` — a new state cell carries the input's symbol, the input becomes the free next-cycle choice), so antecedent and consequent read the same sampled value and the property decides. Before #656 it refused such atoms (it left inputs free, and a formula pinning one would have decoupled antecedent from consequent); `--no-input-state` / `MUNUNU_NO_INPUT_STATE=1` restore that refusal, and the two-player path keeps it. |
| explicit / symbolic cube, `must_edge_inference: Off` | **Sound for HOLDS** (may over-approximation + safety). Cannot refute a data-dependent violation; reports ⊥. |
| explicit / symbolic cube, must-edge inference on | **Sound at the lift (2026-08-28, §6 update box, A–C)** — the vacuous must out of an empty cube that caused the false `Violated` is no longer created, and `assert_must_subset_may` gates any residual. Was **unsound** pre-fix — §6. |
| `symbolic` engine (`symbolic_bitblast::AbstractRelation`) | **Sound (2026-08-28, §6 update box, D)** — an input-predicate abstraction now falls back to may-only (`r_must = None`) instead of building a support-dirty `R_must`; definite HOLDS transfers, refutation → ⊥. e2e-confirmed in `mununu-sva` (this is the path that still panicked after A–C). Was **panic (debug) / wrong verdict (release)** pre-fix — §6. |

## 9. What is argued vs verified vs gap

- **Argued** (§2): the augmentation is a conservative extension. Standard history-variable
  reasoning; no mununu-specific hypothesis.
- **Verified** (§3, §4): the `|=>` / `|->` boundary and the shadow's presence in the
  model are pinned by tests in `tests/past_shadow_input_e2e.rs`, run against live
  slang + yosys in the `mununu-sva` image.
- **Measured** (§6): the two defects, and the ⊥ ceiling for refutation, reproduced
  across four engine postures and two data widths (pre-fix).
- **Gap — CLOSED at the lift (2026-08-28, §6 update box).** `R_must ⊆ R_may` is now
  enforced where must-edges are created: the ∀∃ must is restricted to the emitted
  may-relation (Fix A), proven-empty source cubes are excluded from must-promotion
  (Fix B), and a per-lift `assert_must_subset_may` rejects any residual containment
  violation before the evaluator (Fix C). Mechanism-level unit tests pass; the
  remaining confirmation is the e2e `$past` reproduction in the `mununu-sva` image
  (host runs cannot exercise slang), which should show the §6 `Violated`/`panic`
  entries turn into a sound `holds`/`violated`, and a genuine data-dependent
  violation reach a definite `VIOLATED` rather than ⊥.

## 10. References

- Abadi & Lamport, *The Existence of Refinement Mappings*, TCS 82(2), 1991 — history
  and prophecy variables.
- Bruns & Godefroid, *Generalized Model Checking: Reasoning about Partial State
  Spaces*, CONCUR 2000 — the 3-valued KMTS semantics §5 uses.
- [`kmts-theory.md`](kmts-theory.md) — the lift's own theory notes.
- [`free-input-atoms.md`](free-input-atoms.md) — the non-temporal form of the same
  bindability problem.
- [`predicate-image-soundness.md`](predicate-image-soundness.md) — the abstraction's
  soundness argument this one sits on top of.
