# IR-track P4 / P5 — scoping + cost/benefit verdict (2026-06-23)

> Status: historical — a planning note published 2026-11-08 from the private planning tree (roadmap G4), unchanged except the banner. It records what was planned and measured at the time; the code and the design notes under `docs/design/` are the live truth.

> The plan designs P4 as **measurement-gated**: "P4 will measure the real cost
> of lowering Promela to `StepEval` + `SmtEncode`; that number gates whether P5
> (global `AbstractionType` retirement) is worth it" (`docs/design/sts-ir.md` §7.3).
> This doc is that measurement/scoping step — done by code-grounded investigation
> rather than a full build, because the investigation surfaced a decision the
> user should make before a multi-session build lands.

## TL;DR

**P4 (lower Promela to the STS-IR seam) delivers no verification-power gain, and
P5's headline deletion is blocked.** Recommend **NOT building the full P4/P5
migration**; instead either (A) a minimal flexibility-proof slice if the
architectural-unification value alone is wanted, or (B) re-scope P5 to "migrate
the value-rich frontends' enumeration onto a shared `Enumerate` helper" (a DRY
cleanup, not a type deletion), or (C) defer P4/P5 as not-worth-it and close the
IR-track at P3.

## Finding 1 — Promela is already fully enumerable; `StepEval` gives no gain

The Promela adapter (`adapter/promela/mod.rs`) abstracts data vars via
`promela_var_to_domain` → `FieldDomain::with_range` (BoundedCounter etc.) and
enumerates an explicit cross-product into a CLTS (`create_variable_automaton`).
The STS-IR abstraction stack's *power* (predicate-cube, 3-valued KMTS, CEGAR) is
for state spaces too large to enumerate — bit-vector RTL. Promela's bounded
integers + control states are a finite cross-product; lowering them to
`StepEval` (the **Enumerate** strategy) reproduces the *same* explicit
enumeration through a different code path. **No verdict improves.** And the
`SmtEncode` (predicate-image) half is inapplicable — Promela has no bit-vector
SMT encoding of its transition relation; building one would be a large lift with
marginal real-world need (Promela's idiom is small-data protocol modeling).

So P4's payoff is **architectural unification only** (one enumeration path
instead of two), at a cost the Explore estimated at ~500–800 LOC for a faithful
`PromelaStepEval` (re-expressing Promela step semantics + guard/nondeterminism).

## Finding 2 — P5 cannot delete `AbstractionType` / `FieldDomain` (load-bearing)

The plan + the cross-surface-deadcode-audit assumed P5 retires the global
`AbstractionType` / `FieldDomain` / `state_enum`. **Confirmed false:** they are
consumed by the **BTOR2/SV bit-blast sidecar path**, which is NOT a P4/P5
migration target (BTOR2 *is* the IR ~1:1; its sidecar bounded-counter abstraction
is the Enumerate-strategy field-clamping policy):

- `adapter/btor2/bit_blast.rs:1459` — `resolve_to_field_domain(sig, &ann)` per state cell.
- `adapter/sidecar/mod.rs:49` `resolve_to_field_domain` + `adapter/sidecar/btor2_resolver.rs` — sidecar → `FieldDomain` for BTOR2/SV cells.
- `adapter/btor2/bit_blast.rs:3239-3270` — `AbstractionType::Ignored` injection for cone-dropped cells.

This is the **M.1 (uart_tx) / M.4 (Caliptra) / R46 (pattgen, sysrst)** bounded-counter
fleet. `FieldDomain` + `AbstractionType` therefore **stay** regardless of P4/P5.

`state_enum::enumerate_cross_product` is consumed only by the value-rich
frontends (`ast_extract/state_space.rs` = C-extraction; Promela via the variable
automata) + its own tests. So `state_enum` *could* be deleted **iff** BOTH
Promela AND C-extraction migrate off it — i.e. the full P4 (Promela) **and** an
equivalent C-extraction migration both land. That is the only deletable artifact,
and it is one enumeration utility module — a small prize for a two-frontend
migration.

## Net cost/benefit

| | Cost | Benefit |
|---|---|---|
| **P4** (Promela → `StepEval`) | ~500–800 LOC faithful step semantics + equivalence tests; multi-session | No verdict gain; one enumeration path instead of two for Promela |
| **P4′** (C-extraction → `StepEval`) | comparable, larger (no agent estimate; ast_extract is bigger) | same |
| **P5** (delete the types) | gated on BOTH above | `AbstractionType`/`FieldDomain` **cannot** be deleted (BTOR2/SV sidecar); at most delete `state_enum` (one util module) after both migrations |

The headline P5 deliverable ("retire the global `AbstractionType`/`FieldDomain`/
`state_enum`") is **not achievable as written**. The achievable residue (delete
`state_enum` after a two-frontend `StepEval` migration) is a marginal DRY win for
a multi-session cost.

## Recommendation

1. **Correct the roadmap/audit claim** (done here + in memory): `AbstractionType`/
   `FieldDomain` are permanent (BTOR2/SV sidecar bounded-counter path); they are
   **not** dead code awaiting P5. The cross-surface-deadcode-audit's "only the
   global AbstractionType/FieldDomain/state_enum remain removal candidates" is
   superseded — they have live consumers.
2. **Do not build the full P4/P5 migration by default** — marginal payoff, no
   soundness/verification gain, multi-session cost. This matches the plan's own
   §6.7 measurement-gated / anti-gold-plating discipline.
3. **If the architectural-unification value is wanted**, the smallest honest slice
   is a **flexibility-proof**: one Promela fixture (`mutex_simple.pml`) lowered to
   a `PromelaStepEval`, verified through the Enumerate strategy, asserting verdict
   equivalence with the current direct-CLTS path + recording the LOC/runtime cost.
   That *proves the IR is frontend-agnostic* (the stated P4 goal) without
   committing to migrating the whole adapter or to the (blocked) P5 deletion.

## Decision for the user

P4/P5 as specced are a marginal-payoff, partially-blocked architectural exercise.
Options:
- **(A)** Ship the minimal P4 flexibility-proof slice (one fixture, ~a session) to
  get the cost number + prove frontend-agnosticism, then formally close P5 as
  "types stay; `state_enum` deletion not worth a two-frontend migration."
- **(B)** Skip P4/P5 entirely; mark the IR-track **complete at P3** (the
  verification-unification payoff — verify + CEGAR on one IR/evaluator — is
  already shipped and is the part that mattered). Update docs to record that
  `AbstractionType`/`FieldDomain` are permanent.
- **(C)** Commit to the full Promela + C-extraction `StepEval` migration anyway
  (multi-session) for the architectural-unification value, accepting no verdict
  gain and only a `state_enum` deletion at the end.

**Recommended: (B)** — the IR-track's load-bearing goal (one IR + one evaluator
for the *verification* path) shipped at P1–P3; P4/P5's incremental value does not
justify the cost. (A) is the fallback if a frontend-agnosticism proof is wanted
on the record.
