# R.6 — Controllability-Aware KMTS: Three-Valued Game Abstraction

> Status: historical — a planning note published 2026-11-08 from the private planning tree (roadmap G4), unchanged except the banner. It records what was planned and measured at the time; the code and the design notes under `docs/design/` are the live truth.

> **Status: planning (replanning pass opened 2026-06-08).** This plan reopens the KMTS track. R.1–R.5 built a *single-agent* KMTS stack (one transition relation split into may/must, evaluated by `evaluate_tri`); R.6 extends it to the *two-player* (controller vs environment) setting that synthesis requires, where may/must must be tracked **per player**. Theory anchor: [`docs/design/kmts-theory.md`](../../docs/design/kmts-theory.md) §7. Proof-of-fire anchor: [`docs/design/industrial-value-and-validation-domains.md`](../../docs/design/industrial-value-and-validation-domains.md) §8.5 (V.6) + [`docs/design/proof-by-fire-findings.md`](../../docs/design/proof-by-fire-findings.md).

## §0 Why this is a replanning pass, not a new track

The trigger (2026-06-08 design review): *"for controllability-aware models, are we properly mixing may/must transitions with the ∀-uncontrollable / ∃-controllable assignment?"* The answer was **no** — the two asymmetries live on disjoint, non-composing code paths:

- `evaluate_tri` (the production 3-valued verdict, [`evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs)) implements the controllability axis (the Skolem grouping in `modal_exists`/`modal_forall`) but is **modality-blind** — its `Node::Modal` arm runs two `modal_bits_from_target` passes, neither of which reads `transition.modality()`. Its 3-valuedness comes only from 3-valued *state predicates*, not the may/must *transition* relation.
- The only code that reads `TransitionModality::MayOnly` for verdicts is the parity-game solver (`parity_game_3v_*`), which is **not** the production verdict path and assigns owners by `Box`/`Diamond` only — it has **no** controllability axis.

So the soundness theorems R.5 leaned on (Bruns–Godefroid CONCUR 2000, single-agent) do not cover a controllability-aware KMTS. The governing result is the **three-valued abstraction of *games*** line (de Alfaro–Godefroid–Jagadeesan LICS 2004): may/must per player; a definite controller win needs *must*-moves for the controller and *may*-moves for the environment.

**Latent, not active.** No shipped adapter emits a controllability-aware KMTS today (the BTOR2 KMTS lifter sets no controllability; the synthesis adapters emit `Sharp` everywhere). The gap becomes active the moment R.6.6 ships an adapter that emits both. R.6 must land the sound evaluator (6.3) **before or with** that adapter.

## §0.1 STATUS RECONCILIATION + PO-3 RE-OPEN (2026-06-23)

> **Why this block exists.** A full cube-modal soundness audit
> (`.claude/reviews/cube-modal-soundness/2026-06-23-full-audit.md`) found a
> tracking discrepancy: the master roadmap (`you-are-a-formal-vast-lake.md`
> §Phase 11) records R.6.3–R.6.6 + V.6 as shipped and **slot 1 CLOSED
> 2026-06-10**, but R.6.3 below still reads "(NEXT)". The master roadmap is
> the stale one on *what shipped*; **this plan is the stale one on the
> per-player obligation being open.** Both are corrected here.

**What actually shipped (per the master roadmap commit trail):**

- **R.6.3 (`2c8612b`, 2026-06-09)** — the **may/must modality FILTER**
  (`TransitionModalityFilter { All, MustOnly }`) threaded through
  `modal_exists` / `modal_forall` / bounded variants / `modal_bits_from_target`,
  dispatched per-(kind, must\|may) in `eval_node_tri`'s `Node::Modal` arm.
  **This is the *modality* axis only** — it is the same fix the IR-track P3.4
  work later relied on for `Control::All` soundness. It is genuinely sound and
  tested (verdict-equivalence on Sharp-only; the r6_3 may/must tests).
- **R.6.4 (`0681db0`)** hyper-must cardinality > 1; **R.6.5 (`c071b87`)**
  `OwningPlayer` subgame tag; **R.6.6 (`32944d5`)** controllability-aware
  BTOR2 lifter; **R.6.7 / V.6 (partial)** the hand-authored AMBA-style fixture.

**What was NOT delivered (the re-opened gap):** R.6.3's done-criterion below
has THREE parts. Parts 1 (verdict-equivalence) + 2 (Skolem integration) shipped.
**Part 3 — the per-player preservation argument written against de Alfaro et al.
(kmts-theory §7.3) — was NOT written**, and the planned approach (swap
`modal_trit_from_target` into the `Node::Modal` arm) was **abandoned** in favour
of the filter-on-the-existing-Skolem-path route. Consequently:

- `modal_trit_from_target` / `modal_trit_core` (the per-edge 2×2 per-player
  reference) remain `#[allow(dead_code)]` and are **never verified to agree**
  with the production Skolem `Control::Controllable` / `Control::Environment`
  arms for arbitrary edge configurations (only spot-checked on the small
  `controllable_*` / `integration_*` unit fixtures).
- So the *modality* axis is sound on the production path, but the **composition
  of the modality axis with the controllability (game) axis is unaudited** —
  exactly the de Alfaro LICS 2004 guarantee §0 said was required.

**PO-3 RE-OPENED as R.6.8 (below).** The commit message for `2c8612b` conflated
"the player establishing a *modality* (must-side)" with the *controllability*
per-player rule; they are different axes. The genuine per-player game-soundness
audit is open.

**Blast radius (what this gates, and what it does NOT):**

- It does **NOT** make any *currently-shipped verdict* unsound. The verification
  path (M.x, V.0–V.5) uses bare `Control::All` modalities, which bypass the
  Skolem arms entirely (IR-track P3.4 early-return). V.6 is partial-shipped and
  its verdict-divergence demo rests on the sound may/must filter producing a
  conservative `KleeneBot` — `KleeneBot` is the honest "refine" answer, not an
  over-claim.
- It **DOES** gate any future **DEFINITE controllability verdict** — a
  controller-win `T` or no-controller `F` over a controllability-aware KMTS
  (the V.6 "controller exists" headline, and any synthesis-over-abstraction
  claim). Those rest on the Skolem Controllable/Environment arms being a sound
  realisation of the de Alfaro per-player rule, which is precisely R.6.8.

### R.6.8 — Per-player preservation audit (✅ CLOSED 2026-07-04; RE-OPENED 2026-06-23; was R.6.3 DC#3)

> **✅ CLOSED 2026-07-04.** The soundness FIX shipped 2026-06-23 (the swap-in
> to `modal_trit_from_target` → `modal_trit_core`; see `kmts-theory.md` §7.5).
> This section's remaining gap — the *exhaustive* §7.2-corner property test that
> Route-1/Route-2 both required (the fix rested only on spot-checks + the one
> over-claim corner) — is now discharged by
> `r6_8_modal_trit_core_matches_spec_oracle_exhaustively`
> (`mu_calculus/evaluator.rs`): production `modal_trit_core` == an independent
> from-the-definition `spec_oracle` over all `2^12` edge-type subsets × 6
> (kind × control) corners = 24 576 cases, all agree. `modal_trit_core`'s
> set-valued quantifiers make type-subset enumeration COMPLETE. The de Alfaro
> §7.3 per-player preservation argument is mechanised; DEFINITE controllability
> verdicts may be claimed sound. **This block below is retained for provenance.**

**Done-criterion (pick one route):**
1. **Audit-in-place** — prove (doc-level, against de Alfaro–Godefroid–Jagadeesan
   LICS 2004 / kmts-theory §7.3) that the production Skolem `Control::Controllable`
   / `Control::Environment` arms of `modal_exists` / `modal_forall`, *composed
   with* the R.6.3 may/must `TransitionModalityFilter`, realise the §7.2 2×2
   (controllability × modality) rule. Back it with a property test:
   `production-Skolem+filter == modal_trit_core` over randomised small KMTSes
   spanning all four §7.2 corners × {Sharp, MayOnly, MustHyperOnly} edges. Record
   a `// SOUNDNESS:` block at the modal step.
2. **Swap-in** — adopt the original plan: route `Node::Modal` through
   `modal_trit_from_target` (un-dead-code it), gated on verdict-equivalence with
   the current path on every Sharp-only fixture + the §7.2-corner property test
   above. Heavier (changes a production verdict path → full pre-push workspace
   check incl. doctests) but retires the dead reference.
**Gate.** No `examples/verify/v6_*` (or any other) **DEFINITE** controllability
verdict (`T`/`F`, not `KleeneBot`) may be labelled sound until this closes.
Until then, V.6 stays scoped as "verdict-divergence pattern demo (blind
over-claims → aware returns `KleeneBot`)", never "controller proven to exist".
**Cross-refs:** kmts-theory §7.5 (the boundary, updated 2026-06-23) + the
cube-modal-soundness audit PO-3.

### Items revisited from R.5 (carried forward, now insufficient as-is)

| R.5 item | Status under R.6 | Why |
|---|---|---|
| R.5.0 3v verdict path (`evaluate_tri`) | **Extend** | Modality-blind on the modal step; needs the per-player may/must gate (6.3). |
| R.5.0 sub-item 4.x parity-game owners | **Extend** | Owners assigned by `Box`/`Diamond` only; controllability owners not modeled (6.5). |
| R.5 CEGAR failure-subgame (`FailureSubgame.classifying_transitions`) | **Extend** | No owning-player tag; controllability-aware refinement picks different predicates per owner (6.5). |
| `EdgeModality::MustHyperOnly` (stubbed, treated as `Sharp`) | **Activate** | Controller forcing is set-valued; hyper-must required for completeness (6.4). |

## §1 Sub-items

### R.6.1 — Theory note (DONE 2026-06-08)
[`kmts-theory.md`](../../docs/design/kmts-theory.md) §7: two orthogonal asymmetries, the 2×2 (controllability × modality) rule table, the de Alfaro–Godefroid–Jagadeesan game-abstraction foundation, must-hypertransitions, the implementation-status soundness boundary, and the controllability-aware CEGAR sketch. Reading-list additions in §8.5.

### R.6.2 — `modal_trit_from_target` draft + tests (DONE 2026-06-08)
[`evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs): `EdgeFacts`, the pure `modal_trit_core` (the §7.2 table), and the `EvalContext::modal_trit_from_target` method (CLTS walk + classification). Marked `#[allow(dead_code)]` — staged, not yet wired. Validated by `modal_trit_draft_tests` (11 tests): the `Sharp`-everywhere reduction to §4.3, and the soundness corner (controllable `MayOnly` witness ⇒ `Unknown`, not `True`), both pure and end-to-end through a real KMTS `Clts`.

### R.6.3 — Wire into `eval_node_tri` + per-player preservation audit (PARTIAL — see §0.1)

> **Status (corrected 2026-06-23):** parts 1+2 of the done-criterion below
> shipped as the may/must FILTER (`2c8612b`, 2026-06-09). **Part 3 (the
> per-player preservation argument) was NOT delivered and is RE-OPENED as
> R.6.8 (§0.1).** The filter route was taken *instead of* the
> `modal_trit_from_target` swap named in the done-criterion below; that
> swap remains an option for R.6.8 route 2.
**Done-criterion.** `eval_node_tri`'s `Node::Modal` arm calls `modal_trit_from_target` (single modality-aware pass) instead of the two modality-blind `modal_bits_from_target` calls. Gated on:
1. **Verdict-equivalence invariant** (mirrors R.5.0): on every `Sharp`-everywhere fixture (the R.3 baseline + all current adapters), `evaluate_tri` returns bit-for-bit identical verdicts. The draft's `sharp_everywhere_controllable_has_no_unknown` test is the unit-level guard; the integration guard is the R.3 sweep.
2. **Full label-set Skolem integration**: compose the modality gate with `group_transitions_by_uncontrollable_labels` so the mixed-label set-forcing case (not just the guard-filtered idiom) is exact. The draft's flat classification is the semantic core; this item lifts it onto the existing grouping.
3. **Per-player preservation argument** written against de Alfaro et al. (kmts-theory §7.3), recorded in the commit + a `// SOUNDNESS:` block at the modal step.
**Risk.** This changes a production verdict path → full pre-push workspace check (CLAUDE.md §Pre-push) including doctests; `EvaluationOptions` is untouched (no new field) unless a feature flag is added, in which case the construction-site sweep applies.

### R.6.4 — Activate must-hypertransitions (`MustHyperOnly` cardinality > 1)
**Done-criterion.** `modal_trit_from_target` (and `modal_trit_core`'s `is_must` classification) read the hyper-target *set*, not the single recorded target. A controllable forcing move into a set `{s1,s2}` is a definite witness iff *all* set members are in `must_true(φ)` (set-forcing already half-present on the controllable side via the label grouping). Add a regression fixture with a cardinality-2 hyper-must edge.

### R.6.5 — Controllability-aware CEGAR (owning-player subgame tag)
**Done-criterion.** `FailureSubgame.classifying_transitions: Vec<(usize, usize)>` ([`parity_game_3v.rs`](../../crates/mununu-core/src/mu_calculus/parity_game_3v.rs)) gains an owning-player tag (`Player` or `Control`). The R.5 CEGAR loop branches on it: an uncertain **environment** `MayOnly` edge ⇒ refine to *shrink* `R_may`; an uncertain **controllable** `MayOnly` edge ⇒ refine to *grow* `R_must`. No new fixpoint machinery — abstraction refinement stays separated from game solving (kmts-theory §7.6). `FailureSubgame` is a load-bearing struct → construction-site sweep on the field change.

### R.6.6 — An adapter that emits a controllability-aware KMTS
**Done-criterion.** One adapter emits a CLTS that carries **both** controllable labels **and** `MayOnly`/`MustHyperOnly` edges from the same source — the first model where the R.6.3 evaluator is load-bearing (and where the pre-R.6.3 path is provably unsound). Candidate: extend the BTOR2/predicate-abstraction lifter to a synthesis input (uncontrollable primary inputs vs controllable control signals) with a predicate-abstracted datapath. This is the unlock for R.6.7. **Order constraint:** must not land before R.6.3, or it ships a known-unsound verdict path.

### R.6.7 — Proof-of-fire industrial milestone (V.6)
See §2. Ships `examples/verify/v6_controllability_kmts/` and a `proof-by-fire-findings.md` ledger row with the same honest-result discipline (no Success row without a shipped fixture directory).

## §2 V.6 — Proof-of-fire (industrial case)

**Goal.** Demonstrate, on a real public reactive-controller-synthesis problem, that the controllability-aware modal step (R.6.3) is **load-bearing**: it produces a sound verdict (a definite controller win, or a `KleeneBot` that drives refinement) where the modality-blind path over-claims a controller that the concrete system does not admit.

**Primary candidate — AMBA AHB arbiter (GR(1) synthesis).** The canonical controller-synthesis benchmark (Bloem, Jobstmann, Piterman, Pnueli, Sa'ar — the original GR(1) synthesis line; present in the public SYNTCOMP/TLSF corpus). It has the three load-bearing features at once:
- **Controllability split**: bus requests (`HBUSREQ[i]`) are uncontrollable environment inputs; grants (`HGRANT[i]`, `HMASTER`) are controllable outputs.
- **Data abstraction producing `MayOnly` edges**: the burst-length counter / `HBURST` encoding is the natural predicate-abstraction target; abstracting it produces may-edges the controller cannot rely on.
- **Alternating-fixpoint liveness**: "every request is eventually granted" is a GR(1) response — νμ alternation, the §1 (kmts-theory) class where 2-valued abstraction is unsound and the per-player may/must is required.

**Honest claim to be tested.** *On an abstracted AMBA AHB arbiter, the controllability-aware KMTS evaluator returns a sound synthesis verdict; the modality-blind path returns a spurious "controller exists" because it accepts a `MayOnly` controllable grant as a definite move.* The deliverable is the demonstration of the **divergence** (modality-blind `True` vs controllability-aware `KleeneBot`/refine→`True`-or-`False`), reproduced on a pinned public spec.

**Blocker discipline (same as R.x/M.x).** A V.6 failure downgrades or retracts the claim; it is **not** hand-translated to force a pass. Likely blockers to surface honestly: TLSF→KMTS extraction scale (the §10.2 state-bit cap that blocked Pipelines E/F in proof-by-fire-findings), and whether the public AMBA TLSF abstracts cleanly to a small-enough controllability-aware KMTS. If the AMBA case is blocked at extraction scale, the fallback candidate is a hand-authored CTXDSL controllability-aware KMTS (a 3-client round-robin arbiter with an abstracted credit counter) shipped as a *design-pattern demonstration* (per CLAUDE.md §Claims Integrity — labeled as such, not as a real-system finding), with the AMBA extraction logged as a blocker.

## §3 Sequencing

```
R.6.1 (done) ─┬─> R.6.3 ──> R.6.4 ──> R.6.5 ─┬─> R.6.6 ──> R.6.7 (V.6)
R.6.2 (done) ─┘                               │
                                  (R.6.6 must NOT precede R.6.3)
```

R.6.3 is the critical path: it is the soundness fix and the gate for everything downstream. R.6.4/6.5 harden completeness and refinement. R.6.6 produces the first model that exercises the new path. R.6.7 is the proof-of-fire that validates the whole arc against an industrial case.

## §4 Cross-references

- Theory: [`docs/design/kmts-theory.md`](../../docs/design/kmts-theory.md) §7, §8.5.
- Draft code: `crates/mununu-core/src/mu_calculus/evaluator.rs` (`modal_trit_core`, `modal_trit_from_target`, `modal_trit_draft_tests`).
- Industrial milestone: [`docs/design/industrial-value-and-validation-domains.md`](../../docs/design/industrial-value-and-validation-domains.md) §8.5 (V.6).
- Proof-of-fire ledger: [`docs/design/proof-by-fire-findings.md`](../../docs/design/proof-by-fire-findings.md).
- Single-agent KMTS foundation (R.1–R.5): the verdict path, CEGAR loop, GKMTS stub this plan extends.
