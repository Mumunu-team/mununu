# Interpolation → mununu's KMTS CEGAR — a bridge note

> **Status: planning.** Read this *after* the three external papers below.
> It does not anchor live code as a source of truth; it explains how the
> papers map onto mununu's BTOR2 → KMTS lifter and its CEGAR loop, and it
> names precisely which lines are live, which are MVP, and which are the
> placeholder the papers are meant to fill. Companions:
> [`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md) §4
> (the operational recipe), [`kmts-theory.md`](kmts-theory.md) (the 3-valued
> theory), [`abstraction-literature.md`](abstraction-literature.md) §23
> (Godefroid–Jagadeesan, the 3-valued CEGAR entry this note operationalises).

## 0. The three documents this note follows

1. **McMillan, *Applications of Craig Interpolants in Model Checking*** (TACAS 2005) — *what an interpolant is and why it refines.*
2. **Henzinger, Jhala, Majumdar, McMillan, *Abstractions from Proofs*** (POPL 2004) — *the interpolant becomes the new predicate.*
3. **Dimovski, *Variability Abstraction and Refinement for Game-Based Lifted Model Checking of Full CTL*** (FASE 2019) — *interpolation driving refinement over game-based 3-valued (KMTS-style) checking.*

The order is deliberate: paper 1 gives you the object, paper 2 gives you the 2-valued CEGAR loop that consumes it, paper 3 is the only one that lands the loop in our actual semantic setting (may/must, three truth values, a parity game). This note re-reads all three against mununu's code.

---

## 1. The one-sentence map

mununu already has the entire CEGAR scaffold — the lift, the 3-valued evaluator, the spuriousness discharge, the predicate-set growth, the iteration cap — wired and tested. **The single hole is the function that turns a refuted spurious counterexample into the next predicate.** Papers 1–2 specify that function in the 2-valued world; paper 3 tells you the two things that change when you move it onto a KMTS. Everything else in our loop is already the right shape.

Concretely, the hole is here:

```rust
// crates/mununu-core/src/adapter/btor2/cegar.rs:112
/// **R.5 follow-up.** Compute a Craig interpolant between the
/// concrete-relation states the may-edge admits and excludes.
/// MVP behaviour: returns empty; loop terminates at cap.
CraigInterpolation,
```

`PredicateSource::CraigInterpolation` returns empty today. `PredicateSource::Manual` (a caller-supplied closure) and `PredicateSource::WeakestPrecondition` (a heuristic that just grabs the next uncovered state register — *not* real WP back-substitution, see its own doc comment at [cegar.rs:104](../../crates/mununu-core/src/adapter/btor2/cegar.rs#L104)) are the only working sources. The papers are the spec for promoting that placeholder to a real refinement engine.

---

## 2. Paper 1 (McMillan TACAS 2005) → what we extract, and *from which solver call*

McMillan's framing: given an unsatisfiable conjunction `A ∧ B`, a Craig interpolant `I` is a formula over **only the shared vocabulary** of `A` and `B` with `A ⊨ I` and `I ∧ B ⊨ ⊥`. The model-checking payoff is that `I` is a *relevant* over-approximation of the `A`-side reachable states — exactly specific enough to exclude the `B`-side, no more.

In mununu, the `A ∧ B ⊨ ⊥` that we already produce is the **concrete-discharge UNSAT** in the recipe doc §4.3:

```text
∃ s_0..s_n : s_0 ⊨ b_0 ∧ initial(s_0)
           ∧ ∀i. (s_i, s_{i+1}) ∈ R_concrete ∧ s_{i+1} ⊨ b_{i+1}
           ∧ violation(s_n)
```

When this is UNSAT, the abstract may-trace `b_0 … b_n` is spurious. Split the conjunction at the frontier step `j` where the predicate set first fails to distinguish: `A` = the prefix `s_0..s_j` constraints, `B` = the suffix `s_{j+1}..s_n` + `violation`. The interpolant `I_j` over the shared variables (the state bits live at step `j`) is, by McMillan's theorem, a predicate that (a) every concrete prefix-reachable state satisfies and (b) is inconsistent with the spurious suffix. **`I_j` is the predicate to add to `P`.** That is the whole idea, and it is the cleanest possible answer to recipe doc §4.5's open "extract a candidate predicate via interpolation."

The operational detail McMillan stresses — *interpolants come from the refutation proof, not a second solver call* — is why our integration point is an interpolating SMT backend, not a post-hoc query. We already have the seam: the CEGAR loop's [`craig_refinement_over_transition`](../../crates/mununu-core/src/adapter/btor2/cegar.rs) poses `get-interpolant` over the exact transition (the earlier cube-only `invoke_cvc5_for_interpolant` call was removed in mununu#977). cvc5's `get-interpolant` (and Z3's `SMT-LIB get-interpolant` / `Interpolant` API) is exactly McMillan's "read it off the proof" packaged as a solver command. The R.5-follow-up work is wiring that return value into a `PredicateSpec`, not implementing proof-theoretic interpolation ourselves.

**What to take from paper 1:** the interpolant is *the* principled replacement for the WP heuristic. WP gives you a predicate that is locally correct but blind to relevance (it'll happily add the whole cone); the interpolant is minimal-by-construction over the shared frontier. When you read §4.5 of the recipe doc afterwards, read "interpolant" as "the McMillan object extracted at the spurious frontier `j`."

---

## 3. Paper 2 (Abstractions from Proofs, POPL 2004) → the loop we already built

This is the paper whose loop mununu's `cegar_refine_loop` *is*. Henzinger et al.'s contribution over plain CEGAR (Clarke–Grumberg–Jha–Lu–Veith 2000) is precisely the marriage of §2 to the refinement step: instead of guessing a separating predicate, extract it as an interpolant from the infeasibility proof of the spurious path, **at each cut point along the path** — yielding one predicate per program location rather than one global predicate. That per-location locality is what makes the method scale and terminate in practice.

Map to our code:

| Abstractions-from-Proofs concept | mununu artifact (live) |
|---|---|
| Abstract reachability / model check | `evaluate_3v_game_with_options` over the lifted `Clts` |
| Spurious abstract path | `CegarIteration` abstract counterexample (lasso or prefix), recipe §4.2 |
| Path-infeasibility proof | concrete-discharge UNSAT, recipe §4.3 ([cegar.rs](../../crates/mununu-core/src/adapter/btor2/cegar.rs) discharge query) |
| **Per-location interpolant → new predicate** | `PredicateSource::CraigInterpolation` — **the placeholder** |
| Add predicates, re-abstract | `predicate_cube_lift(P ∪ {I_j})`, the eager lifter (R.2.5), [cegar.rs:547](../../crates/mununu-core/src/adapter/btor2/cegar.rs#L547) |
| Bounded iteration / progress | `CegarOptions::max_iterations` (default 16), `CegarTermination` |

The single most useful thing to carry from paper 2 into our codebase: **interpolate at every cut point of the spurious trace, not just one.** Our discharge query is a bounded-unrolling of the may-trace, so it has exactly the sequence of cut points Henzinger et al. interpolate at (`I_0, I_1, … I_{n-1}`, one per unrolled step). Harvesting the whole sequence in one refinement round is what gives the loop its empirical termination; harvesting one predicate per round (the naive reading) is what makes CEGAR oscillate. Recipe doc §6.3 cause-1 ("the interpolant repeats from earlier rounds → the loop is oscillating") is the failure mode this paper's per-cut-point harvesting is designed to avoid. When you wire `CraigInterpolation`, return `Vec<PredicateSpec>` from the cut sequence, not a single predicate.

One honest caveat the paper forces: interpolation-based CEGAR terminates for safety over finite-state systems *given a finite interpolation language*. Over bit-vectors that's automatic (finite domain), so our BTOR2 path inherits termination — but the *quality* of termination (how many rounds) depends on the interpolating solver's interpolant choices, which mununu does not control. That is the right thing to surface in a `refinement_trace.json` (recipe §6.2) so a stalling run is diagnosable.

---

## 4. Paper 3 (Dimovski FASE 2019) → the two things that change on a KMTS

Papers 1–2 live in the 2-valued world: one transition relation, refinement triggers on a spurious **`false`** (a reachable error state in the over-approximation). mununu does not live there. Our evaluator is the **3-valued game** over a **KMTS** with separate may/must edges, and refinement triggers on **`KleeneBot` (⊥)**, not on `false`. Dimovski is the one paper of the three that runs interpolation-based refinement in *this* setting — game-based, 3-valued, may/must — so it is the one that tells you what to adjust. Two adjustments, both already anticipated by our design docs:

### 4.1 You refine the *indefiniteness*, not the *violation*

In 2-valued CEGAR the refinement target is unambiguous: a `false` verdict with a spurious witness. In the 3-valued game the evaluator can return `KleeneT`, `KleeneF`, or `KleeneBot`, and **only `KleeneBot` is a refinement trigger** — `KleeneT`/`KleeneF` are definite and transfer to the concrete by Bruns–Godefroid (kmts-theory.md). The counterexample you discharge is therefore not "a path to an error" but "a path the game could not classify" — the may-trace through the indefinite region, recipe §4.2.

The consequence for interpolation: `A`/`B` are split at the frontier where the verdict went `⊥`, i.e. where a may-edge exists but the corresponding must-edge does not, so the box/diamond modality could not commit. This is exactly Dimovski's game-based refinement: the spurious object is a *non-winning, non-losing* play in the abstraction game, and the interpolant is extracted to *split the abstract state so the play resolves to a win or a loss*. Read his "refinement of the game graph" as "add the interpolant predicate so the `⊥`-cube splits into cubes that carry a definite must-edge or definitely lack one."

`abstraction-literature.md` §23 (Godefroid–Jagadeesan, *Automatic Abstraction Using Generalized Model Checking*, TACAS 2003) is the same idea stated as a recipe and is the entry our `refine` function cites; Dimovski is the more recent, more explicitly *interpolation*-driven instance of it. Read §23 of the literature doc and Dimovski together — they are the 2-valued-CEGAR-lifted-to-3-valued pair.

### 4.2 Refinement has a second axis: must-edges, not just predicates

This is the part with no analogue in papers 1–2, and it is where mununu has genuinely live, recent code. A `KleeneBot` can have *two* causes:

1. **State-distinguishability gap** — the predicate cube is too coarse; two concretely-distinct states are merged. Fix: add a predicate (the interpolation story above). This is the only axis 2-valued CEGAR has.
2. **Missing-must-edge gap** — the states are fine, but the lift failed to *prove* a must-edge that concretely exists, so a diamond/`◇` obligation that should be `KleeneT` sits at `⊥`. Fix: establish the must-edge, not a new predicate.

Recipe doc §4.4 calls this "unsat-core partitioning — predicate refinement vs UF concretisation," and the second axis is live today as `MustEdgeInference`:

```rust
// crates/mununu-core/src/adapter/btor2/kmts_lift.rs:350
pub enum MustEdgeInference {
    Off,                  // default — preserves prior may-only behaviour
    SamplingConfluence,   // sampling-derived must / hyper-must edges
}
```

The `SamplingConfluence` post-pass (commits `54a274c`, `cf1aaca`, `8273244`, 2026-06-06) promotes may-edges to `Sharp` and emits `MustHyperOnly` edges from sampled `(input, state)` confluence — a *cheap, unsound-until-proven* must-edge oracle, explicitly soundness-tagged as **candidate** until the SMT-backed must-proof lands ([kmts_lift.rs:846](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L846)). The interpolation work and the must-edge work are the two prongs of recipe §4.4's partition: an UNSAT core mentioning *bitvector constants* → interpolate a predicate (axis 1); a core mentioning *operator/must-witness terms* → an `R_must` proof or UF concretisation (axis 2).

**The thing to internalise from Dimovski for our codebase:** interpolation alone is *not sufficient* in the 3-valued setting the way it is in the 2-valued setting. A `⊥` from a missing must-edge will not be cleared by *any* new predicate — you can split cubes forever and the diamond still can't commit. Recipe §4.7's stall detection ("a round produces neither a new predicate nor a new UF concretisation") is the guard for exactly this: it catches the case where the loop keeps interpolating against an indefiniteness whose real cause is on the must-axis. So the operational rule is: **partition the core first, then choose the axis.** Papers 1–2 give you axis 1; Dimovski tells you axis 2 exists and must be checked first when the `⊥` sits on a `◇`/must obligation.

---

## 5. Putting it together — the refinement step as it should land

After the three papers, the `CraigInterpolation` source should become:

```text
refine_step(spurious may-trace τ through a ⊥-region):
    proof := concrete_discharge(τ)            # recipe §4.3
    if proof is SAT:  return KleeneF(witness) # real cex — Dimovski: a losing play
    core := unsat_core(proof)
    (const_terms, must_terms) := partition(core)        # recipe §4.4
    preds := []
    if const_terms nonempty:                            # axis 1 — papers 1+2
        for each cut point j along τ:                   # paper 2: per-location
            preds.push(interpolant(A_j, B_j))           # paper 1: McMillan object
    must_jobs := []
    if must_terms nonempty:                             # axis 2 — Dimovski/§4.4
        must_jobs.push(prove_must_edge | concretise_uf) # MustEdgeInference path
    if preds.empty() and must_jobs.empty():
        return Stall                                    # recipe §4.7
    return Refine(preds, must_jobs)
```

Every line of that already has a home in the codebase or the recipe doc; the only net-new engineering is the `interpolant(A_j, B_j)` call (today `craig_refinement_over_transition`, decoded by `parse_interpolant_to_predicate_expr`) and the partition heuristic. The papers are what let you write those two pieces with a soundness argument instead of a guess.

## 6. Reading-order recap

- Read **paper 1** for the object; then re-read recipe §4.5 — "interpolant" now has a precise meaning.
- Read **paper 2** for the loop; then re-read [cegar.rs](../../crates/mununu-core/src/adapter/btor2/cegar.rs) `cegar_refine_loop` and notice it *is* this loop with one stubbed function.
- Read **paper 3** for the 3-valued/game adjustments; then read `abstraction-literature.md` §23 and recipe §4.4 — the two-axis partition and the `⊥`-not-`false` trigger are the whole delta from papers 1–2.
- The keystone you are building toward is promoting `PredicateSource::CraigInterpolation` from "returns empty" to the `refine_step` above.

### Honest scope note

There is no single published source that does *all* of "Craig interpolation + KMTS + two-axis (predicate ∥ must-edge) refinement." Papers 1–2 are 2-valued; Dimovski is 3-valued and game-based but does not carry the must-edge/UF second axis in the form mununu needs (that axis traces to Andraus–Sakallah's Reveal and Godefroid–Jagadeesan, recipe §7.3 and lit §23). The synthesis — interpolation on axis 1, must-edge/UF on axis 2, both gated by an UNSAT-core partition over a 3-valued `⊥` trigger — is mununu's own contribution, not something to be found pre-assembled in the literature. That is worth stating plainly in any write-up, per the repo's claims-integrity discipline.
