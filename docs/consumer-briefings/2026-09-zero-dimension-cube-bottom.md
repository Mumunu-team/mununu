# Consumer briefing — 2026-09 properties that only ever returned `unknown` now decide

> **Audience:** monono, ROSF, anyone running `mununu sv verify-auto` or reading its verdicts.
>
> **Related:** [mununu#503](https://github.com/Mumunu-team/mununu/issues/503). Interacts with [mununu#377](https://github.com/Mumunu-team/mununu/pull/377) (the unsatisfiable-cube mask), which is unchanged.
>
> **TL;DR:** a whole class of properties — those whose atoms are *all* derived labels — was returning `unknown` **regardless of what the property asserted**, including tautologies. Fixed. **Verdicts change in one direction only: `unknown` → `holds` or `violated`. No definite verdict flips.**

## What was wrong

An atom over an input or an input-derived combinational signal is deliberately **not** a sound cube dimension — its value depends on the demonic environment, so it is labelled per-cube by SMT instead. That is correct and stays.

But a property whose atoms are *all* of that kind seeds **zero cube dimensions**, and three shipped behaviours then composed into a wrong one:

1. `|P| = 0` ⇒ exactly one cube: the universal set, holding every concrete state.
2. **Every** may-edge path was gated on `!predicates.is_empty()`, so that cube was emitted with **no edges** — and no labels either, because the `step` label is interned inside the skipped blocks.
3. `downgrade_unsatisfiable_cells` uses *"no outgoing edges"* as a proxy for *"unsatisfiable cube"*, and masks every edgeless cell to ⊥.

So the verdict never depended on the property. Measured on the real sysrst lift — same design, same formulas, only the dimension count changed:

| cube dimensions | `nu X. ([] X)` | `trigger_i != trigger_active` (a tautology) |
|---|---|---|
| **0** (the shipped shape) | `unknown` | `unknown` |
| **1** (one dimension added) | **`holds`** | **`holds`** |

`nu X. ([] X)` contains **no atom at all** and is true at every state of every KMTS. That it returned `unknown` is what showed the cause was structural rather than a binding failure.

## What changed

The guards were relaxed so `|P| = 0` is an ordinary case. **The ⊥ mask was not weakened.**

That distinction matters: the mask's proxy rests on the invariant *"a cube lifted from a total transition relation is edgeless only if it is empty"*, and the defect was that the guards broke that invariant. Restoring it means the mask keeps protecting the spurious νμ-`violated` case it exists for (the i2c `AG EF` incident, #377). Both of its pinning tests pass unchanged.

At `|P| = 0` the edge computation reduces to a single `0 → 0` check — cheap, and **exact**: a total design yields the self-loop; a design whose `constraint` admits no transition correctly yields none, and ⊥ there is right.

## Direction of change — read this bit

- `unknown` → `holds`, or `unknown` → `violated`.
- **No `holds` becomes `violated`, and no `violated` becomes `holds`.**

`violated` is in that list deliberately. The must-edge emitters live inside the same guard, so `<>` / `EF` properties in this class also become decidable — not just `[]` / ν ones. If a property in this class was masking a real violation, you will now see it.

**If you pin expected verdicts**, expect some `unknown` rows to become definite. A row that changes is a property that was never actually being checked.

## Who is affected

| Path | Effect |
|---|---|
| A property whose atoms are **all** input-derived (e.g. `a != b` where both trace to inputs) | **Decides now.** Previously always `unknown` |
| A property with **at least one** state-cell atom | **None** — it already had ≥1 dimension |
| `btor2 cegar` / `verify` on a sidecar with only `derived: true` compounds | Same improvement |
| The exact-symbolic engine (`--engine exact-symbolic`) | **None** — it does not use the cube path |

## Verification

```bash
cargo test -p mununu-core --lib -- zero_dimension_lift downgrade_unsatisfiable unsatisfiable_cube_cell
```

Three new regressions, all of which **fail on the pre-fix code**; plus the two tests pinning the #377 mask, which pass unchanged. 2526 host lib tests green.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | verdict semantics on the cube path | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump; the e2e runs here | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no cube path | No |
| rosf | consumes verdicts | **Yes if it pins expected verdicts**, else No |
| monono Docker | formal lane pins verdicts in `verify.sh` | **Yes** |
| mununu-ui | no type change | No |

## For monono

Your `verify.sh` files pin expected verdicts, so this is the change most likely to move a row for you. Any property that flips out of `unknown` is one that was previously not being checked at all — worth reading rather than just re-pinning.

## Not covered here (follow-ups)

- **`e2e_sysrst_detect_real_sva_verdict_breakdown` still fails**, on a *different* assertion (`sva_12`, `unknown_cells: 4`) over a **non-degenerate** cube space — so a different cause, not this defect. Tracked under #503.
- **`e2e_sysrst_config_concretization_flips_timer_relationals`** likewise (`unknown_cells: 32`, 5 dimensions). Untouched.
- The `predicate_image_pending` flag is still set for a zero-dimension lift; it now has edges, so this is cosmetic, but it was not audited here.
