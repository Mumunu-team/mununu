# Consumer briefing — 2026-10 the exact member no longer abstains on its own fixpoint's garbage

> **Audience:** monono, ROSF, and any consumer that reads `btor2 verify` / `verify-recoverability` / `verify-liveness` / `sv verify-auto` outcomes, pins `--expect NAME=unknown`, or reads `decided_by`.
>
> **Provenance:** found while instrumenting category 5 of the engine-performance roadmap (budgets); fixed in the same PR. Regression test `exact_bad_reachable_is_not_cut_by_the_fixpoints_garbage`. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR — some `unknown`s become `holds` / `violated`; nothing flips between the two definite values

The `exact-symbolic` member (full-state ROBDD over OxiDD, bit-blast μ-fixpoint) built the initial-state BDD **after** running the fixpoint. Its node-budget guard counts allocated-including-dead nodes, so the fixpoint's garbage — correct, finished, already summed into a verdict — was charged to the one small bit-blast that followed, and the member reported `abstained on the NODE budget` on a cone it had just decided (measured on the parent commit with the regression test's 16-bit cone: `2752513 of 2097152 live BDD nodes`, where the fixpoint itself fits in a fraction of that). It only worked when `MUNUNU_BDD_REPORT_PEAK` happened to run a collection in between: an instrument that changed the verdict.

The initial-state BDD is now built before the fixpoint at all three affected sites. A property whose only decider was the exact member can move from `unknown` to a definite verdict. A property that was already decided by another member keeps its value and may change its `decided_by`.

## What changed

Three call sites, one pattern:

| site | surface |
|---|---|
| `exact_bad_reachable` | the owned reach portfolio's exact member (`btor2 verify`, `sv verify-auto` safety) |
| the abstract two-player game (`btor2 game` on the predicate abstraction) | game verdicts |
| `exact_symbolic_verdict_with_witness_inner` | every exact-member μ-calculus verdict with a witness (`verify-recoverability`, `verify-liveness`, the ⊥ re-plan) |

`BddBitBlaster::initial_state_bdd` runs before `ExactModel::evaluate` at each. The guard itself (`check_node_budget`) is unchanged and carries an `ORDERING HAZARD` comment: it is a start-of-op check on allocation volume, so **any bit-blast placed after a fixpoint is charged for the fixpoint**. A gc-then-recheck variant of the guard was tried and reverted — it made the wide-design abstention tests take 344–387 s, because the collection ran on every budget hit of a cone that was going to abstain anyway.

## What to update, per consumer

### monono

- **Expect fewer `unknown`s on the exact path.** Any `--expect NAME=unknown` pin recorded on a property where the note said `abstained on the NODE budget` may now fail because the property decides. Re-run and re-pin; a move `unknown → holds/violated` on this upgrade is this change.
- **A move `holds ↔ violated` is NOT this change** and is worth reporting with the design.
- `decided_by` can change from a slower member (native BMC, interpolation, a subprocess) to `exact-symbolic` on the same value.

### ROSF

- No code change: outcome vocabulary and shape are unchanged. The distribution shifts (fewer `unknown`), which moves any dashboard keyed on it.

### Report-parsing impact

None. `PropertyVerdict` fields, the JSON schemas and the outcome vocabulary are unchanged; no schema regeneration.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | verdict values: exact-member `unknown` → definite on affected cones | **Yes** |
| `mununu-dev` | test image; carries the regression test | **Yes** |
| `mununu-sva` | extends `mununu-dev`; same change on the SVA path | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The regression: a 16-bit twocount forward reach that abstained on the parent and decides now.
cargo test -p mununu-core --lib -- exact_bad_reachable_is_not_cut_by_the_fixpoints_garbage

# 2. A verbatim reproducer from your own corpus (the design that moved):
MUNUNU_PROBE_BTOR2=/path/to/design.btor2 \
  cargo test -p mununu-core --lib -- probe_exact_bad_reachable_verbatim --ignored --nocapture

# 3. Re-run any corpus that pinned `unknown` with an `abstained on the NODE budget` note and diff
#    the outcome column. unknown -> definite is this change; holds <-> violated is not.
```

## Not covered here

- The guard's semantics. `check_node_budget` still measures allocation volume, not live nodes; a future bit-blast placed after a fixpoint will hit the same hazard. The comment at the guard names it; a structural fix (charging per op, or collecting once at fixpoint exit when a bit-blast follows) is not in this PR.
- `MUNUNU_BDD_REPORT_WORK` (same PR) is a diagnostic line and does not change any verdict; it is documented in `CLAUDE.md`'s environment table.
