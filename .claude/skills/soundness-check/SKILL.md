---
name: soundness-check
description: >
  Checks adapter, abstraction and engine code for soundness-documentation
  compliance: every evaluation fallback, abstraction default and dropped
  construct carries a `// SOUNDNESS:` comment naming its direction; one
  direction per model; the KMTS path for alternating properties; the
  differential-oracle registration of every new engine member; the
  propose-then-check rule for learned components; and the CLTS / CTXDSL
  capability-use rule. Use when asked to audit soundness annotations;
  invoked by review-orchestrator.
---

Perform a soundness audit of $ARGUMENTS (with no arguments, the branch against `main` plus the working tree, `.rs` files). The rules: `CLAUDE.md` §Soundness Guarantees, §Abstraction Guidelines, §Adapter / Emitter Capability Use; theory and the per-subsystem recipe in `docs/abstraction.md` and `docs/design/kmts-theory.md`.

mununu's verdicts are correct for the model; transfer to the real system depends on the abstraction's direction and the property's alternation. Two-valued abstraction is sound only at the lattice corners (safety + over-approximation; liveness + under-approximation); an alternating fixpoint (νμ, GR(1) response, nested obligations) needs the KMTS 3-valued path, where definite verdicts transfer at every depth and `KleeneBot` triggers refinement. Every information-dropping decision is therefore written down where it is made.

## Audit checklist

1. **Evaluation fallbacks.** The evaluators are named `evaluate`, `evaluate_guard`, `evaluate_pure*`, `eval_op`, `eval_node`, `eval_const` (under `adapter/`, `abstraction/`, `guard/`, `context_dsl/`). Every path on which one returns `None`, a default (`unwrap_or`, `unwrap_or_default`, `unwrap_or_else`), or `true` / `false` for "cannot evaluate" carries a `// SOUNDNESS:` comment within 3 lines that names the direction — over-approximation (admits the transition / behaviour) or under-approximation (blocks it) — and the property class it is conservative for.

2. **One direction per model.** Within one emitted model the fallbacks agree in direction, or the mixing is declared in one place with its consequence. A νμ / GR(1) / `AG EF` property evaluated on a 2-valued (`Sharp`-everywhere) model gets a MEDIUM: the verdict carries only the corner guarantees and the report must say so; the fix is the KMTS path (`Transition` modality in `crates/mununu-core/src/clts/mod.rs`, `Clts::state_3valued_predicates`; the worked example is `adapter/btor2/kmts_lift.rs`).

3. **Abstraction defaults.** Every default (`DEFAULT_COUNTER_BOUND` and the `AbstractionType` variants — `Boolean`, `Presence`, `BoundedCounter`, `EnumValues`, `Ignored` — in `adapter/domain.rs`, a bit cap, a `mem { x : shared }` tag) has a comment saying why it is sound for the property class, and a soundness regression test with the abstract case and one concrete case in the same class.

4. **Dropped constructs.** A parser or lift arm that skips a construct (a default `match` arm that `advance()`s, an unsupported SVA feature, a cut module) emits an adapter warning or a diagnostics note the consumer reads (`ModelDiagnostics` / `VerificationNote` in `adapter/slang/verify_auto.rs`, the lift error's named module) **and** a `// SOUNDNESS:` line stating what the drop admits or forbids. Whether the note reaches the report is `/failure-path-audit`'s question; the direction is yours.

5. **Nondeterminism / havoc.** Several targets for one label, an input left free, an out-of-cone array havocked: a comment says it is an intentional over-approximation and for which properties that is conservative.

6. **Capability under-use** (`CLAUDE.md` §Adapter / Emitter Capability Use; `docs/abstraction.md` §"Multi-label transitions are synchronisation vectors"). Flag re-encodings of what the CLTS / CTXDSL layer already carries: parallel single-label edges where a multi-label edge fits; predicate values suffixed onto state names where `state_variable_bitset` / `predicates { … }` fit; controllability in label-name prefixes where `LabelControllability` fits; LTL where a `[(req_next = {…})]` guard is direct. And the inverse: `transition s -> t on a, b` is a **synchronisation vector** (fires on `{a, b}` together, pairs only with a set-identical partner under composition — an unmatched compound freezes the automaton, mununu#570); "any one of these" is `on a | b`; a composition that admits unmatched vectors declares `allow_dead_vectors;`. LOW severity; an intentional under-use needs a one-line comment.

7. **Black-box / contract chaotic-stub discipline.** An adapter meeting a module without a body (yosys `(* blackbox *)`, an instantiated module with no definition, a software call with no summary — `CallEffect` in `adapter/extraction/ast_extract/call_summary.rs`) calls `contract::discover::build_blackbox_sidecars` (`crates/mununu-core/src/contract/discover.rs`) or the lift's auto-stub, **and** emits a structured `tracing::warn!` / diagnostics entry naming the module, the gap kind, the labels affected and the consequence. The slang lift's two stub forms differ in meaning and both are named in `blackboxed_modules`: `(* mununu_missing_module *)` empty stubs become anonymous inputs (over-approximation); `(* blackbox *)` stubs are deliberate cuts. Silently producing transitions on an un-bodied module, or omitting it, is HIGH; loud loss without a sidecar is MEDIUM.

8. **Codesign coupling.** `crates/mununu-core/src/codesign/{coupling,compose}.rs` emit `CompositionKind::Asynchronous` (`context_dsl/ast.rs`) for a HW/SW composition; a synchronous one-step rendezvous is unsound for racy bus access. Pinned by `coupling_fragment_uses_asynchronous_composition`. HIGH.

9. **Discharge graph circular acceptance.** `crates/mununu-core/src/contract/discharge.rs`: a non-trivial SCC yields `CircularWithRankWitness`, `Circular` (sign-off required) or `PotentiallyCircular` — never `Acyclic`, never dropped. HIGH.

10. **Differential-oracle registration.** A new engine member, reduction or rescue (`adapter/reach_portfolio.rs`, `adapter/reach_rescue.rs`, `planner/mod.rs`) is cross-checked by the merge: a disagreement degrades to ⊥ with a soundness alarm, never a guess. A member whose verdict can reach `outcome` without the contradiction check is HIGH. An interrupted or cancelled query is never read as a verdict (`QueryGate`, #879); a wall-clock stop may yield ⊥, never a definite.

11. **Learned components propose, the engine checks** (§Soundness Guarantees, 2026-11-05). Anything a model, an LLM or a learned heuristic emits — a property, a predicate hint, a repair, a routing change — reaches `outcome`, `vacuity` or a trace only after the checker confirms it. A path that lets a proposal through unconfirmed is HIGH.

12. **Formula inversion.** Fixpoint-variable references inside a body are never negated when a formula is inverted; the dual fixpoint's starting point carries the semantics (§Code Reuse, Dead Code, Performance, Security). A negated bound variable oscillates and is HIGH.

13. **Non-vacuity.** A `holds` on an SVA row produced on the bare host (no slang, no yosys `read_slang`) is presumed vacuous until reproduced in the `mununu-sva` image; a new property class carries its dual (`EF(target != v)` reachable) or says why not.

## Search patterns

```
(evaluate|evaluate_guard|evaluate_pure|eval_op|eval_node|eval_const)[^;]*(None|unwrap_or)   # item 1
unwrap_or\(|unwrap_or_default\(\)|unwrap_or_else\(                                          # items 1, 3
=> \{\s*(self\.)?advance\(\)|_ => \{\}|unsupported                                           # item 4
labels:\s*vec!\[[^,\]]+\]|controllable_labels:\s*vec!\[\]                                   # item 6 (adapter files)
CompositionKind::(Synchronous|Superset)                                                     # item 8 (codesign files)
Acyclic                                                                                     # item 9 (discharge.rs)
```

## Output format

Per finding: **path:line** · checklist item · severity — HIGH (no comment, wrong direction, or a HIGH item above) / MEDIUM (a comment without a direction; a 2-valued model under an alternating property) / LOW (clearer wording; capability under-use) · the `// SOUNDNESS:` text or code change proposed.

## Examples

```rust
// SOUNDNESS: over-approx — a guard the evaluator cannot decide admits the transition; more
// behaviours than the design has, conservative for safety, may produce a spurious liveness ⊥.
let guard_holds = evaluate_guard(&expr).unwrap_or(true);
```

```rust
// BAD: no direction stated
// Default to 3 if not specified
let bound = field.bound.unwrap_or(3);
```
