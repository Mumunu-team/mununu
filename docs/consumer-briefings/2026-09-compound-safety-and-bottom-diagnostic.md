# Consumer briefing — 2026-09 compound-safety rescue + `bottom-reason` diagnostic (closes mununu#492)

> **Audience:** monono (primary reporter — their `mem_router` contrast pair depends on this), ROSF (API consumer via `--profile industrial`), any orchestrator gating on `sv verify-auto` verdicts and needing to distinguish `unknown` variants.
>
> **Related:** [mununu#492](https://github.com/Mumunu-team/mununu/issues/492) — the ticket. Complements mununu#490 (process-memory ceiling; distinguishes "ran out of memory" from "engine did not decide") by adding the third distinction: "the property shape doesn't fit the rescue lane" vs "the engine gave up."
>
> **TL;DR:** two changes to the `sv verify-auto` ⊥-escalation. **(A)** The safety-rescue lane now decides `AG(compound_boolean_of_single_atoms)` — exclusion (`!a || !b`), implication (`!a || b`), conjunctive (`a && b`) — with leaves resolved through state cells / output nets / **primary inputs** (the last is essential for zero-state / stateless models). The ticket's `mem_router_faulty` case now returns `VIOLATED` instead of `UNKNOWN`. **(B)** For any property that stays ⊥ after escalation, a new `bottom-reason` verification note classifies why — `safety-shape-not-reducible`, `no-state-model-non-safety`, or `unclassified`. Purely additive; wire format unchanged; consumers observe a new note kind on residual-⊥ properties.

## What changed

### Part A — compound-atom safety escalation

- **New reducer** in [`crates/mununu-core/src/adapter/reach_rescue.rs`](../../crates/mununu-core/src/adapter/reach_rescue.rs): `reduce_ag_boolean_body(formula) -> Option<NodeId>` — accepts `nu X. (COMPOUND && [] X)` where `COMPOUND` is `And`/`Or`/`Not`/`True`/`False`/`Predicate`, and each `Predicate` leaf parses as `PredicateExpr::Cmp` (single register-comparison). Rejects `CmpReg` / `CmpRegAddend` / `Select` leaves and any modal / fixpoint / variable inside `COMPOUND`.
- **New emitter** in [`crates/mununu-core/src/adapter/btor2/bad_monitor.rs`](../../crates/mununu-core/src/adapter/btor2/bad_monitor.rs): `emit_ag_boolean_invariant_monitor` — walks the mu-calc subtree, compiles each leaf `Cmp` to a BTOR2 comparison node, combinators to `and`/`or`/`not` BTOR2 nodes, and emits `bad = !compound_expr`. Signal-resolution fallback is state → output → **input** (the input fallback is new — it's what makes zero-state models decidable without a state cone).
- **Wiring** in `reach_portfolio_rescue`: try the existing single-atom reducer + emitter first (preserves byte-for-byte behavior on the shipped case); fall back to the compound path when the single-atom lane returns None or the single-atom emitter refuses the signal.

### Part B — `bottom-reason` diagnostic

- **New classifier** in `crates/mununu-core/src/adapter/slang/verify_auto.rs`: `classify_bottom_reason` recognizes three shapes:
  - `SafetyShapeNotReducible` — Safety-class property that neither reducer accepts.
  - `NoStateModelNonSafety` — non-Safety (liveness / recoverability / mixed / reachability) property on a design with zero state registers.
  - `UnclassifiedBottom` — none of the above; the ⊥ is unexplained by this classifier (most commonly a resource abstain the engine already flagged).
- **New `bottom-reason` verification note** — one per residual-⊥ property, kind `"bottom-reason"`, `ScopeCaveat` level, `summary` and `detail` naming the classification + actionable guidance.
- Attached at the tail of `escalate_bottom` so any consumer of `sv verify-auto` sees it in the response `verification_notes` array.

## What did NOT change

- **Wire format** — 17/17 schema drift tests pass with zero regeneration; `SvVerifyAutoResponse` unchanged; verification-note SHAPE unchanged (a new `kind` string appears on residual-⊥ properties).
- **`PropertyVerdict` values** — same canonical vocabulary. Verdicts that were `unknown` under Part A's addressable shapes are now `violated` or `holds` — a soundness upgrade, not a wire change.
- **CLI surface** — no new flags.
- **Existing rescue paths** — single-atom safety, liveness l2s, fair-cycle l2s, recoverability all unchanged in behavior. Two pre-existing tests updated (they asserted `notes.is_empty()` after a Liveness-class ⊥ escalation; they now assert the presence of a `bottom-reason` note instead).

## For monono, ROSF, and any lane consumer

**What to expect on adoption:**

- **Verdict upgrades**: any `sv verify-auto` invocation on a design with compound-atom safety properties (especially on stateless / zero-state models) may see previously-`unknown` verdicts flip to `holds` / `violated`. For monono's `mem_router` contrast pair: the faulty twin's `sva_20` now returns `violated` instead of `unknown/⊥`. **This is a soundness upgrade** — the property was always violable; the escalation just didn't reach it.
- **New note kind**: `verification_notes[i].kind == "bottom-reason"` appears on any property that stays ⊥. To distinguish resource-abstain from shape-mismatch, parse the summary:
  - `"safety-shape-not-reducible"` — property reshape needed (or a new reducer).
  - `"no-state-model-non-safety"` — modelling issue (compose with a stateful context or restrict to tier-1 safety).
  - `"unclassified"` — look at sibling engine notes (`bit-cap`, `abstained on the …`) for the actionable answer.

**A downstream gate that treated all ⊥ interchangeably** (rejecting everything as "not decided here") continues to work unchanged — a `bottom-reason` note is additive information, not a verdict override. **A downstream gate that wants to act on the distinction** now can: e.g., `safety-shape-not-reducible` should NOT be retried with a bigger budget (it will still fail); `unclassified` MAY be worth retrying with a raised bit-cap / node-budget.

**Soundness contract:**

- The compound-atom rescue is sound. `AG(compound)` is universally quantified over all trajectories AND input schedules; the emitted `bad = !compound` is reachable iff there exists a reachable state × input assignment falsifying the invariant — a genuine counterexample transferring unchanged to the design.
- Zero-state models work at k=0: with no state cone to walk, the reachability portfolio decides in one SAT query.
- The `bottom-reason` note is diagnostic-only; it never overrides a verdict.
- Non-goals (still ⊥ after this PR):
  - Compound with `CmpReg` register-vs-register leaves (`AG(a == 1 && b == c)`) — classified `safety-shape-not-reducible`.
  - Non-Safety on zero-state models — classified `no-state-model-non-safety`; compose to add state or restrict to tier-1 safety.
  - `AG(a → AX b)` implication-with-next shape — separate reducer, out of scope.

**Docker rebuild:** binary bump only; no subprocess tool changes.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | binary picks up the widened safety rescue + `bottom-reason` classifier; verdicts on compound-atom safety on stateless models may flip from `unknown` to `violated`/`holds` | Yes if consumers pin a version tag; the flip is a soundness upgrade |
| mununu `Dockerfile.dev` | binary bump + 11 new reducer/e2e tests + 3 classifier tests | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; no e2e slang-gated behaviour change (existing `#[ignore]`d `e2e_rescue_*` tests still #[ignore]d) | No — the SVA e2e set is unaffected |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `Dockerfile` / `Dockerfile.dev` / `.hw` | consumes mununu CLI/API; may see verdict upgrades on stateless blocks — this is what the industrial-profile lane WANTS | No — behaviour updates on next binary pull |
| monono Docker (if any) | primary beneficiary — `mem_router` faulty twin flips to `violated`, contrast pair becomes real | No — pull the new binary and re-run the contrast lane |
| mununu-ui deployment | no impact — note kinds are string data | No |

## Verification steps

- `cargo test -p mununu-core --lib adapter::reach_rescue::` — 20/20 (11 new + 9 preserved), including 4 end-to-end verdict tests on a hand-crafted zero-state fixture (`ZERO_STATE_MODEL`) covering exclusion, implication, tautology, and conjunction shapes.
- `cargo test -p mununu-core --lib --features api escalate_bottom` — 10/10 (2 updated to assert `bottom-reason` presence).
- `cargo test -p mununu-core --lib --features api bottom_reason` — 3/3 classifier tests (compound-safety gets decided rather than classified; safety-shape-not-reducible classified correctly; no-state-model-non-safety classified correctly).
- `cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1` — 17/17 schema tests pass, zero regeneration.

## Provenance

- Fix commit: (pending merge — branch `fix/492-compound-safety-and-bottom-diagnostic`).
- Ticket: [mununu#492](https://github.com/Mumunu-team/mununu/issues/492).
- Design record: `.claude/plans/492-compound-safety-and-bottom-diagnostic.md`.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- Related shipped work: mununu#490 (process-memory ceiling — distinguishes "ran out of memory" from "engine did not decide"; this PR adds the third distinction "shape doesn't fit the rescue lane").

## Not covered here (documented non-goals)

- **Compound-with-CmpReg leaves** (`AG(a == 1 && b == c)`). Requires a broader compilable-leaf set on the emitter; classified `safety-shape-not-reducible` today.
- **Non-Safety on zero-state models.** Response-liveness's l2s and recoverability's cube both need state to reason over; the diagnostic classifies these as `no-state-model-non-safety` with a modelling recommendation.
- **`AG(a → AX b)` implication-with-next shape.** A separate reducer; not the ticket.
- **A stateless-model fast path** that compiles the entire formula into a boolean-of-inputs SAT query. Part A + the existing portfolio handle it via k=0 reachability without a special path — simpler.
