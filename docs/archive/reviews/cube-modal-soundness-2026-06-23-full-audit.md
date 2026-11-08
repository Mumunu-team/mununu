# Predicate-Cube Modal-Soundness Audit (full matrix)

> Status: historical — a planning note published 2026-11-08 from the private planning tree (roadmap G4), unchanged except the banner. It records what was planned and measured at the time; the code and the design notes under `docs/design/` are the live truth.

> **✅ PO-3 CLOSED 2026-07-04.** The per-player finding was fixed 2026-06-23
> (swap-in to `modal_trit_from_target` → `modal_trit_core`; `kmts-theory.md`
> §7.5) and is now **exhaustively** validated:
> `r6_8_modal_trit_core_matches_spec_oracle_exhaustively`
> (`mu_calculus/evaluator.rs`) confirms `modal_trit_core` == an independent
> from-the-definition oracle over all 24 576 achievable edge configurations
> (2^12 type-subsets × 6 kind×control corners). The §7.3 per-player
> preservation argument is mechanised; DEFINITE controllability verdicts are
> sound. The PO-3 sections below are retained for provenance.
>
> Analysis only — no code changes. Authored 2026-06-23 in response to:
> *"do we need to look further into the cube labels? are we satisfying all
> box/diamond versions (i.e. controllable, uncontrollable, particular
> label)? can we prove those are sound and that mu-calculus satisfaction is
> preserved?"*
>
> Scope: the predicate-cube path (`adapter/btor2/kmts_lift.rs::predicate_cube_lift`
> → 3-valued `KleeneDom` evaluation in `mu_calculus/evaluator.rs`), reached
> from `btor2 cegar` (CLI), `/api/v1/btor2/cegar` + `/api/v1/sv/cegar` (API),
> and the `/cegar` panel (UI). This is the path M.4 and V.6 ride on.

---

## Bottom line

The question has a one-paragraph answer and a long one.

**One paragraph.** Of the full matrix `Control ∈ {All, Controllable, Environment}
× {Box, Diamond} × {must, may} × {bare, label-specific, bounded}`, exactly
**one slice is audited-sound over the cube: `Control::All`, bare (label-agnostic)
modalities, unbounded.** Its production semantics provably match the published
Bruns–Godefroid preservation theorem (the derivation is in §4), it is regression-
tested, and the P3.4 fix (2026-06-22) closed the one place it was wrong. Every
other slice is either **vacuous** (label-specific modalities over a single-label
cube), **unaudited** (controllability modes — the production Skolem path is
explicitly flagged as not-yet-sound for controllability-aware KMTS in
`kmts-theory.md` §7.5), or **3-valued-blind** (bounded modalities skip the
may/must filter). And even the sound slice rests on **two premises we assert by
construction but have not mechanically established**: that the cube's `may`
relation over-approximates and its `must` relation under-approximates the
concrete design. So: yes, we should look further; no, we cannot today *prove*
"all box/diamond versions" sound — most of them are not yet in scope, and the one
that is has open proof obligations (PO-1, PO-5).

**The honest claim that *is* defensible today:** mununu's cube path soundly
decides **label-agnostic, single-agent (verification) mu-calculus** — including
alternating fixpoints — over a predicate abstraction whose may/must relations are
sound by construction of the SMT lifter. That is exactly what M.4 exercises
(`<>(p5||p6||p7)`, bare diamond, `Control::All`) and is the only framing M.4/V.0–V.5
should make. Controllability-aware verdicts (V.6) are on the *unaudited* slice and
must be labelled as such until PO-3 closes.

---

## §1 — What "sound" means here (the reference)

The governing theorem is the one in [`docs/design/kmts-theory.md`](../../../docs/design/kmts-theory.md)
§4.3 (modal semantics) + §4.5 (preservation), i.e. Bruns–Godefroid CONCUR 2000.
The **standard KMTS modal semantics** (single-agent, label-specific) is:

```text
⟦[a]φ⟧(s) = T  iff  ∀ s' with (s,a,s') ∈ R_may  : ⟦φ⟧(s') = T
            F  iff  ∃ s' with (s,a,s') ∈ R_must : ⟦φ⟧(s') = F
            ⊥  otherwise

⟦⟨a⟩φ⟧(s) = T  iff  ∃ s' with (s,a,s') ∈ R_must : ⟦φ⟧(s') = T
            F  iff  ∀ s' with (s,a,s') ∈ R_may  : ⟦φ⟧(s') = F
            ⊥  otherwise
```

The **preservation theorem** (§4.5): a definite verdict (`T`/`F`) on the abstract
KMTS transfers to the concrete Kripke structure at *every* alternation depth,
**provided** the abstraction is a sound KMTS of the concrete — concretely the two
refinement clauses the proof leans on:

- **may-step accommodation** — every concrete `a`-successor corresponds to some
  abstract `may`-`a`-successor (i.e. `R_may` over-approximates `R`); the load-
  bearing premise for `[a]φ = T` and `⟨a⟩φ = F`.
- **must-step preservation** — every abstract `must`-`a`-edge witnesses a concrete
  `a`-successor for every concretisation (i.e. `R_must` under-approximates `R`);
  the load-bearing premise for `[a]φ = F` and `⟨a⟩φ = T`.

Three things to note up front, because they bound the entire audit:

1. **The theorem is stated for `Control::All`, label-specific modalities.** There
   is no controllability axis in §4.3/§4.5.
2. **The controllability axis is a *different* theorem** — three-valued
   abstraction of *games* (de Alfaro–Godefroid–Jagadeesan LICS 2004), written up
   in `kmts-theory.md` §7. §7.5 ("Implementation status — the soundness boundary")
   states plainly: *"The verdict path that today is correct for the single-agent
   KMTS (verification adapters) is **not automatically** correct for a
   controllability-aware KMTS; it needs the per-player may/must of de Alfaro et
   al."* That sentence is the spine of finding C below.
3. **The theorem assumes the premises hold.** It says nothing about whether *our
   cube* satisfies may-accommodation / must-preservation — that is the lifter's
   job, and it is finding E below.

---

## §2 — What the cube actually is (the structure being evaluated)

From `predicate_cube_lift` (`adapter/btor2/kmts_lift.rs`):

- **States** = predicate cubes over `2^|P|` (the `p5/p6/p7`-style atoms), 3-valued-
  labelled in `state_3valued_predicates`.
- **Labels** = **one shared label, `step`** (interned at `kmts_lift.rs:961/1694/1829`),
  on *every* transition. There is no per-action label structure: all concrete
  transitions of the design collapse onto `step`.
- **Controllability** = `step` is **`Controllable` by default**. Labels with no
  explicit controllability default to `Controllable` at build time (confirmed by
  the `default_label_controllability` test in `clts/mod.rs`; the cube never calls
  `set_label_controllability` on `step` in the plain path). The *only* time the
  cube emits a meaningful controllability partition is the **R.6.6 opt-in path**
  (`AdapterOptions::controllable_inputs` non-empty, `kmts_lift.rs:1783+`), which
  emits dual `[env_cN, ctrl_cM]` labels with explicit `Uncontrollable`/`Controllable`
  tags — that is the V.6 use case, not the M.4 verification use case.
- **`may` relation** = `MayEdgeInference::SmtAllPairs` (Z3 ∃∃ over-approx) or
  sampling; claimed sound over-approximation.
- **`must` relation** = `MustEdgeInference::SmtPerTarget` (∀∀), `SmtPerTargetStandard`
  (∀∃), `SmtHyperMust` (∀ src ∃ input ∃ t∈T), or sampling/confluence; the SMT
  forms claimed sound under-approximation. `Sharp` = both; `MayOnly` = may-not-must;
  `MustHyperOnly{targets}` = a forcing move into a *set*.

The single load-bearing structural fact: **the cube is label-blind (one `step`)
and controllability-trivial-by-default (`step` = Controllable artifact).** Both
facts drive the findings.

---

## §3 — The full matrix

Each cell is the production evaluation (`mu_calculus/evaluator.rs`) and its
soundness verdict *over the cube*. "SOUND" = matches §1's theorem and is tested;
"VACUOUS" = always-trivial, sound-but-useless; "UNAUDITED" = routes through code
not verified against any preservation theorem; "BLIND" = ignores the may/must
distinction the 3-valued soundness needs.

| Control | Modality | Label form | Production path | Verdict over the cube |
|---|---|---|---|---|
| **All** | `<>` / `[]` | **bare** (≡ `<step>`/`[step]`) | `modal_exists` plain-∃ early path (P3.4 fix, evaluator.rs:1780) / `modal_forall` plain-∀; per-(kind,must\|may) filter via `KleeneDom::modal_image` | **SOUND** (matches §1; tested; see §4). Conditional on PO-1 + PO-5. |
| **All** | `<a>` / `[a]`, `a≠step` | label-specific | same, but `guard_matches` filters out every transition (no `a`-edge exists) | **VACUOUS / UNSOUND** — `<a>φ`→F, `[a]φ`→T regardless of the design. See finding B. |
| **Controllable** | `<>` / `[]` | bare | Skolem grouping (`modal_exists`/`modal_forall` lines 1788–2204) over `step`=Controllable | **UNAUDITED + MEANINGLESS over a verification cube** — `step` is an arbitrary default; the Skolem path is §7.5-flagged. See finding C. |
| **Controllable** | `<>` / `[]` | bare | same, over an **R.6.6 controllability-aware cube** | **UNAUDITED** — meaningful partition, but exactly the §7.5 not-yet-sound path. The open V.6 boundary. See finding C. |
| **Environment** | `<>` / `[]` | bare | dual Skolem (`modal_exists` 1990+, `modal_forall` 2205+) | **UNAUDITED** — same as Controllable; §7.5; "rare, inversion-produced". See finding C. |
| **any** | `<…>k` / `[…]k` | **bounded** `steps=k` | `modal_*_bounded*` (2237–2627) with `TransitionModalityFilter::All` (R.6.3 *deferred*, code comment 2257–2258) | **BLIND** — does not apply the must/may filter; 3-valued soundness not established. See finding D. |
| **any** | any | label `req_next`/`forb_next`/`req_cur`/`forb_cur` | `GuardPartitions` pre-filter (1751/1784/2106…) | inherits the row's verdict; the predicate-guard axis is orthogonal and sound *as a state filter*, but composes only with the SOUND row. |

Underneath every row, the may/must (3-valued) layer is computed the same way:
`KleeneDom::modal_image` (evaluator.rs:1376) runs the chosen `modal_exists`/
`modal_forall` **twice** — once `MustOnly`-filtered on `target.must_true()`
(→ `must_bits`), once `All`-filtered on `target.may_true()` (→ `may_bits`) for a
diamond (dual filters for a box) — and combines them into a `TritSet`. So the
Control/Skolem logic of each row applies *under each filter*; the filter is the
only thing that makes the 3-valued distinction. That is why "bounded" is BLIND
(it forces `All` on both) and why the R.6.3 filter fix is what makes `Control::All`
SOUND.

---

## §4 — Why the one SOUND cell is sound (the derivation)

This is the cell the user can rely on. Claim: post-P3.4, the production
`Control::All` bare-modality evaluation **computes exactly §1's standard KMTS
semantics**. Derivation, mapping `modal_image`'s two calls to §1:

Diamond `⟨⟩φ`:
- `must_bits` = `modal_exists` with `MustOnly` filter on `must_true(φ)`
  = `∃ must-edge with ⟦φ⟧ = T`  →  this is **`⟦⟨⟩φ⟧ = T`** of §1. ✓
- `may_bits` = `modal_exists` with `All` filter on `may_true(φ)`
  = `∃ may-edge with ⟦φ⟧ ≠ F`  =  `¬(∀ may-edge ⟦φ⟧ = F)`  →  the negation of
  **`⟦⟨⟩φ⟧ = F`** of §1. ✓ (TritSet reads `must=T ⇒ T`, `¬may ⇒ F`, else `⊥`.)

Box `[]φ` (dual filters):
- `must_bits` = `modal_forall` with `All` filter on `must_true(φ)`
  = `∀ may-edge ⟦φ⟧ = T`  →  **`⟦[]φ⟧ = T`** of §1. ✓
- `may_bits` = `modal_forall` with `MustOnly` filter on `may_true(φ)`
  = `∀ must-edge ⟦φ⟧ ≠ F`  =  `¬(∃ must-edge ⟦φ⟧ = F)`  →  negation of
  **`⟦[]φ⟧ = F`** of §1. ✓

The plain-∃ / plain-∀ shape (no Skolem) is the P3.4 fix: before it, `modal_exists`
applied synthesis Skolem sub-grouping by full label set, and since the cube has
one shared `step` label, *every* successor fell in one group, collapsing `⟨⟩φ`
into `[step]φ` ("all step-successors satisfy"). That manufactured a definite `F`
the KMTS does not justify — the M.4 unsoundness. The fix returns plain
existential for `Control::All` (evaluator.rs:1780-1787), leaving Skolem only for
`Controllable`/`Environment`.

Hyper-must (`MustHyperOnly{targets}`, cardinality > 1) is handled in this cell by
`transition_target_in_set_diamond` (ANY-of-targets) / `_box` (ALL-of-targets)
(evaluator.rs:76/94) — the R.6.4 fix — which is the correct GKMTS forcing-set
reading (Shoham–Grumberg). Tested by `r6_4_hyper_must_helpers_read_all_targets`.

**Tests that lock this cell** (`evaluator.rs` ~4890–5182, `tests/predicate_3valued_*`):
`p3_4_all_diamond_is_existential_over_shared_label` (the exact cube scenario:
two `step`-edges to `T` and `F` states → `<>p = T`, the anti-collapse guard);
`r6_3_evaluate_tri_mayonly_diamond_is_unknown_at_source` (MayOnly into `T` → `⊥`,
not `T`); `r6_3_evaluate_tri_mayonly_box_false_is_unknown` (box dual); the
`r6_4_*` hyper-must cardinality tests; `r3_kleene_baseline` (KleeneDom = BoolDom
on Sharp-only).

**Caveat — tested, not proven.** The mapping above is an argument; it is checked
by examples, not mechanically. The corresponding proof obligation is PO-5.

---

## §5 — The gaps (everything that is not the §4 cell)

### B. Label-specific modalities are vacuous over the cube — and unsound if used.
The cube has one `step` label. A property `<a>φ` / `[a]φ` for any `a ≠ step`
matches no transition: the diamond gets no must-edge and no may-edge → `F`; the
box ranges over the empty set → `T`. Vacuously, this is "sound" only in the
degenerate sense that there is no `a`-action *in the cube*. But the concrete
design *does* have distinct actions — they were all collapsed to `step` by the
lift. So a user who writes a label-discriminated property gets a definite verdict
that reflects the abstraction's label-erasure, not the design. **The cube
soundly supports only the label-agnostic fragment** (`<>`/`[]`, equivalently
`<step>`/`[step]`). This is an expressiveness boundary masquerading as a verdict.
The cube cannot answer "after action `a` …" questions at all.

### C. Controllability modes are unaudited; over a verification cube, meaningless.
Two layers:

1. **Semantics layer.** `Control::Controllable`/`Environment` route through the
   Skolem `modal_exists`/`modal_forall` arms (∀-uncontrollable / ∃-controllable
   grouping). `kmts-theory.md` §7.5 states this path is *not automatically* sound
   for a controllability-aware KMTS — it needs per-player may/must (de Alfaro
   LICS 2004), whose drafted implementation is `modal_trit_from_target` /
   `modal_trit_core`, still `#[allow(dead_code)]`. **The production Skolem path
   and the clean per-edge reference are not verified to agree** for
   Controllable/Environment (they *are* spot-checked to agree on the small
   `controllable_*` / `integration_*` unit fixtures, but that is example coverage,
   not the per-player preservation audit §7.5 asks for). PO-3.

2. **Structure layer.** Over a *verification* cube (`controllable_inputs` empty),
   `step` is `Controllable` purely by the build-time default. So
   `<ctrl=controllable>φ` is asking a game question of a model with an arbitrary,
   non-meaningful player partition — the verdict is not just unaudited, it is
   semantically empty. Over an *R.6.6 controllability-aware* cube the partition is
   real, but then the verdict path is exactly the §7.5 not-yet-sound one. Either
   way, controllability verdicts over the cube are **not** currently defensible.
   This is the open boundary for V.6.

Note the P3.4 fix already insulates the common case: bare `<>`/`[]` parse to
`Control::All`, which now *bypasses* Skolem entirely — so the `step`-is-Controllable
artifact has no effect on verification queries. The exposure is only when a
formula explicitly carries `ctrl = controllable | environment`.

### D. Bounded modalities skip the 3-valued filter.
`modal_exists_bounded` / `modal_forall_bounded` / `_controllable` (evaluator.rs
2237–2627) pass `TransitionModalityFilter::All` (the R.6.3 must/may filter is
explicitly deferred — code comment at 2257–2258). So a bounded `<…>k` / `[…]k`
over a KMTS with `MayOnly` edges does not distinguish may from must, and the
3-valued preservation argument of §4 does not carry. Bounded-modality verdicts
over the cube are unaudited for soundness. (Tracked as R.6.3.b in the roadmap.)

### E. The may⊇concrete / must⊆concrete premise is asserted, not established.
§1's theorem is conditional on may-accommodation + must-preservation. For the
cube these are *claimed* — `SmtAllPairs` may as a sound over-approx, ∀∃/∀∀ must as
a sound under-approx — and argued "by construction of the predicate-image lifter."
But there is **no test or proof that the constructed cube relation actually
satisfies the premise** (e.g. a check that every concrete BTOR2 transition is
covered by some cube `may`-edge, and every cube `must`-edge is realised concretely).
If a lift bug dropped a may-edge or over-claimed a must-edge, §4's soundness
silently fails — the evaluator would be correctly computing §1 over an *unsound*
KMTS. This is the deepest obligation: PO-1.

### F. (rolled into PO-5) Code ≡ §4.3 is argued, not mechanised.

---

## §6 — Proof obligations

What it would take to move each gap from "asserted" to "established." Ordered by
soundness leverage.

- **PO-1 (premise check — highest leverage).** Establish that
  `predicate_cube_lift` produces a KMTS satisfying may-accommodation +
  must-preservation w.r.t. the BTOR2 relation. Two routes: (a) a *proof* that the
  SMT query shapes (`SmtAllPairs`, `SmtPerTargetStandard`) imply the premises —
  largely a matter of writing down the ∃∃ / ∀∃ query semantics and matching them
  to §1; (b) a *differential test* on small fixtures: enumerate the concrete
  BTOR2 transition relation, the lifted cube, and assert `R ⊆ γ(R_may)` and
  `R_must ⊆ ᾱ(R)`. (b) is cheap and catches lift regressions; (a) is the durable
  artifact. Closing PO-1 is what upgrades §4's verdict from "sound *if* the lift
  is sound" to "sound".

- **PO-5 (code ≡ semantics).** Promote the §4 derivation to either (a) a doc-
  level proof in `kmts-theory.md` (the filter-table → §4.3 mapping, written once,
  cited), or (b) a property test: for random small KMTSes, assert the production
  `KleeneDom` modal verdict equals a direct evaluation of §1's four clauses. Low
  effort, high confidence, and it pins the filter dispatch against future edits.

- **PO-2 (label boundary — guard, not proof).** No proof is possible (the cube
  *has* erased labels); the obligation is to *prevent the unsound use*. Either
  reject label-specific modalities (`guard.labels` referencing anything but `step`)
  at realize-time over a cube with a `// SOUNDNESS:` error, or document the
  label-agnostic restriction at the `btor2 cegar` / `/cegar` surface. Analysis
  recommendation, not implemented here.

- **PO-3 (controllability per-player audit).** The §7.5 obligation: verify the
  production Skolem `modal_exists`/`modal_forall` (Controllable/Environment arms)
  implements the de Alfaro LICS 2004 per-player may/must rule — i.e. prove (or
  property-test against `modal_trit_core`) that the Skolem grouping = the per-edge
  2×2 reference for *all* edge configurations, not just the unit fixtures. Until
  PO-3 closes, V.6 controllability verdicts are labelled unaudited. This is the
  largest obligation and the one gating the V.6 headline.

- **PO-4 (bounded 3-valued).** Thread `TransitionModalityFilter` through the
  `modal_*_bounded*` family (R.6.3.b), then extend PO-5's property test to bounded
  modalities. Until then, bounded modalities over a non-Sharp cube are out of the
  sound fragment.

---

## §7 — Recommendations (analysis; no code changed)

1. **State the sound fragment explicitly, everywhere the cube is exposed.** The
   defensible claim is *"label-agnostic, single-agent mu-calculus, unbounded
   modalities, over a may/must abstraction sound by construction."* M.4's docs
   already do this honestly (pre = definite hazard, post = ⊥). The `btor2 cegar` /
   `sv cegar` help text and the `/cegar` panel should carry the same one-line
   restriction so a user does not write `<a>` / `<ctrl=…>` / bounded queries
   expecting sound verdicts.

2. **Close PO-1 and PO-5 first** — they are cheap (differential test + property
   test + a doc paragraph) and they convert the M.4/V.0–V.5 story from "sound if
   the lift is sound, by an unmechanised argument" to "sound, tested end-to-end."
   These are the highest soundness-per-effort items and have no dependency on the
   R.6 game track.

3. **Gate controllability + label-specific + bounded modalities over a cube**
   (PO-2 path; realize-time rejection or warning) until PO-3/PO-4 close. This is
   the difference between "unsound if misused" and "cannot be misused." It also
   makes the dead-feature audit's V.6 boundary explicit at the surface.

4. **Do not advance the V.6 controllability headline past "unaudited" until
   PO-3.** This is consistent with `kmts-theory.md` §7.5 and the roadmap's
   R.6 ordering (6.3–6.6 shipped, but the per-player preservation audit is the
   open item). Claims-integrity: a controllability-aware "controller exists"
   verdict over the cube is, today, a design-pattern demonstration, not a finding.

---

## §8 — Direct answers to the question asked

> *do we need to look further into the cube labels?*

Yes. The single `step` label is the root of two findings: it makes label-specific
modalities vacuous/unsound (B), and its Controllable-by-default tag routes
controllability modalities through an arbitrary partition (C). The cube is sound
only for label-agnostic modalities.

> *are we satisfying all box/diamond versions (controllable, uncontrollable,
> particular label)?*

No. Of the matrix in §3: **`Control::All` box+diamond, bare** is satisfied
(audited-sound, §4). **Particular-label** box/diamond are vacuous/unsound (B).
**Controllable/uncontrollable (Environment)** box/diamond are unaudited and, over
a verification cube, meaningless (C). **Bounded** versions are 3-valued-blind (D).

> *can we prove those are sound and that mu-calculus satisfaction is preserved?*

For the `Control::All` bare-modality fragment: the preservation theorem (Bruns–
Godefroid, `kmts-theory.md` §4.5) *is* the proof, and §4 shows our code computes
its semantics — but the proof is conditional on PO-1 (the lift actually produces
a sound may/must KMTS), and the code≡semantics step is PO-5 (argued, not yet
mechanised). Close PO-1 + PO-5 and that fragment is provably sound end-to-end.
For the other fragments (controllability, label-specific, bounded) we cannot prove
soundness because they are not yet in the sound design — they need PO-2/PO-3/PO-4,
of which PO-3 (the de Alfaro per-player game audit) is the substantive one and
gates V.6.

---

## §9 — Addendum 2026-06-23: PO-5 shipped; it surfaced PO-6

**PO-5 closed (this session).** `po5_kleene_modal_matches_bruns_godefroid_4_3`
(`evaluator.rs`, in `modal_trit_draft_tests`) enumerates every 2-edge KMTS over
{Sharp, MayOnly} × {T, F} target labels (16 fixtures × {◇, □} = 32 checks) and
asserts the production `evaluate_tri` verdict at the source equals the §4.3
reference computed inline. Green. This mechanises the §4 derivation for the
audited-sound cell (`Control::All`, bare, definite-predicate labelling — the cube
shape). All three verdicts are exercised; `⊥` arises from the `MayOnly` edge
modality, exactly as the cube produces it.

**PO-6 (NEW, gated — surfaced *by* PO-5).** The first PO-5 draft additionally
ranged over `KleeneBot` *state predicates* and found a real discrepancy:

> `[a]p` over a target whose predicate `p` is `KleeneBot` returns definite **`F`**
> in production, where §4.3 says **`⊥`** (`F` needs `∃ must-edge → p=F`; a `⊥`
> target is not definitely-`F`). The box *may*-side collapses a `⊥` state
> predicate to `F` — an **unsoundness** (claims `[a]p` definitely fails when the
> abstraction can't justify it).

**Currently unreachable, hence gated — not fixed here.** The predicate-cube lift
assigns only `KleeneT`/`KleeneF` to state predicates — *"No `KleeneBot` values are
produced"* (`adapter/btor2/kmts_lift.rs:19`, 207); a cube fixes each predicate, so
the cube's 3-valuedness is entirely in the may/must *edge* relation, never in `⊥`
state labels. No current production path emits a `⊥` state predicate, so PO-6 does
not affect any shipped verdict (M.4 included). PO-5 is therefore scoped to T/F
predicate labels (the cube's actual shape).

**The gate.** `kmts_lift.rs:33` notes a *future* predicate-image refinement step
that *will* introduce `KleeneBot` state valuations. **PO-6 must close before that
ships** — i.e. fix the box *may*-side (and audit the diamond dual) so a `⊥` state
predicate yields `⊥`, not `F`. The likely site is the atom→`TritSet` construction
(`predicate_bits` / `KleeneDom` atom eval): ensure `may_true` includes `⊥`-labelled
states. Until ⊥ state predicates are emitted, this is latent.

### Proof-obligation ledger (updated)

| PO | What | Status |
|---|---|---|
| PO-1 | cube `may ⊇ concrete` / `must ⊆ concrete` premise (differential test) | **CLOSED 2026-06-23** — `po1_cube_brackets_concrete_{deterministic_toggle,nondeterministic_gap}` (`kmts_lift.rs`): lifts (SmtAllPairs may + ∀∃ must), builds the concrete cube relation from `simulate_one_step`, asserts `concrete ⊆ may` ∧ `must ⊆ concrete`; the counter case exercises `must ⊊ may` |
| PO-2 | gate label-specific / `ctrl=` / bounded modalities over a cube | **CLOSED 2026-06-23** — `mu_calculus::cube_modality_soundness_warnings` (pure, unit-tested) wired into `cegar_refine_loop` (once, iter 0); emits an `ApproximateTranslation` warning per out-of-fragment modality (controllability ⇒ PO-3/R.6.8 unaudited; bounded ⇒ PO-4/R.6.3.b; non-cube label ⇒ vacuous). A WARNING not a reject (V.6's ctrl cube keeps working). Rides the existing `warnings` channel ⇒ parity CLI(print)+API(`Btor2CegarResponse.warnings`)+UI(`CegarTraceView`). e2e-verified via CLI. Follow-up: the verify-cube path (P3.3) is a separate consumer not yet wired |
| PO-3 | de Alfaro per-player game audit (= R.6.8) | **CLOSED 2026-06-23** (mununu #139) — audit carried out, **empirically confirmed the over-claim** (controllable `MayOnly` + uncontrollable `Sharp` both into def-good ⇒ `evaluate_tri` returned definite `True` for `<(ctrl=controllable)>p` where de Alfaro gives `Unknown`). FIX: `KleeneDom::modal_image` dispatches `Control::{Controllable,Environment}` (unbounded) → `modal_trit_from_target` (the §7.2 per-player rule, now LIVE; EdgeFacts carry kind-aware `reaches_must/may` via `transition_target_in_set_{diamond,box}` ⇒ hyper-must handled; next-state partition applied). `Control::All` + bounded stay on the audited two-pass. Covers BOTH verdict paths (verify + CEGAR, which delegates to `evaluate_tri`); synthesis (BoolDom) untouched. Test `po3_controllable_diamond_mixed_corner_is_unknown_on_production_path`; full mununu-core suite green (1994/0). PO-2 ctrl-warning re-worded (sound now; advisory kept for the structural verification-cube-default caveat). Docs: kmts-theory §7.5 Resolution, abstraction.md, recipe §4.9, literature #29, V.6 README |
| PO-4 | bounded-modality 3-valued filter (R.6.3.b) | OPEN |
| PO-5 | code ≡ §4.3 (enumerated conformance test) | **CLOSED 2026-06-23** |
| PO-6 | `⊥` state predicate → box *may*-side collapses to `F` | OPEN — gated on ⊥-state-predicate emission (`kmts_lift.rs:33`) |

**End-to-end soundness chain now mechanised for the audited-sound fragment.**
PO-1 (the cube is a sound may/must KMTS) + PO-5 (the evaluator computes §4.3 over
any KMTS) compose to: a definite verdict from the cube path transfers to the
concrete design (§4.5 preservation). Both are tests, run in CI. The remaining
open POs are out-of-fragment (PO-2/PO-3/PO-4) or gated/unreachable (PO-6).
