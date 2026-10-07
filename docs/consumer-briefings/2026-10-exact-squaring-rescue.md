# Consumer briefing — 2026-10 the exact engine decides long-chain reachability by squaring: iteration-budget `unknown`s become verdicts

> **Audience:** monono, ROSF, and any consumer that pins `--expect NAME=unknown` on properties the exact engine abstained on with `abstained on the ITERATION budget`, reads `decided_by`, or sizes `MUNUNU_BDD_ITER_BUDGET` for raster / counter designs.
>
> **Provenance:** category 2 (algorithmic) of the engine-performance roadmap, item A2. Measured first on a feasibility spike (`probe_a2_iterative_squaring_on_the_raster`), then on the heat corpus and the calibrated cases. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR — `unknown` → `holds` / `violated` on the iteration-bound class; nothing flips between definite values

The exact engine's `EF` / `AG` fixpoints walk the design's diameter one pre-image per iteration, which is why a raster (`iters ≈ 1.95 · h · v`) or a wide counter abstained on the 2^20 iteration budget. A pure-reachability fixpoint that is still iterating after 4,096 steps now hands its work to a **squaring closure**: the transition relation is built in a second BDD arena and closed by iterative squaring in ⌈log₂ diameter⌉ relational products; `EF p` is read off the closure. It is the same least fixpoint, computed exactly; the iteration's answer, where it had one, is unchanged.

| design | before | after |
|---|---|---|
| raster 800×8000, 1280×720 (720p), 1920×1080 (1080p) — `AG EF (vcount == last)` | `unknown`, abstained on the ITERATION budget after ~3 s | **decides** in 6–11 ms of closure (22–24 squarings, relation 130–152 nodes) |
| raster 800×200 | 818,627 iterations | 4,098 iterations (the threshold) + the closure |
| twocount 2^17 (`AG EF done`), forward 2^16 (`EF bad`) | 262,146 / 131,072 iterations | 4,098 / 4,097 |
| 40-bit FSM-gated counter (2^40 diameter) — `AG EF (fsm == 2)` | exact engine abstained; the b2 counter-abstraction decided | **the exact engine decides**; b2 agrees |

## What changed

- `ExactModel::fixpoint` recognises the pure shapes — `μX. p ∨ ◇X` and `νX. p ∧ □X` with a bare single-agent modality on `X` and `p` closed (no fixpoint variable free). Game modalities (`ctrl=…`), guarded modalities and bodies that mention an outer variable keep iterating.
- After `SQUARING_AFTER_ITERS` (4,096) iterations of such a fixpoint, `Squarer` builds `R(s, s') = ∃i. constraint(s, i) ∧ ⋀ s'ⱼ ↔ fⱼ(s, i)` in its own OxiDD manager (three interleaved copies of the state bits in the engine's level order, inputs after them), transferring the engine's next-state BDDs; then `T ← T ∪ ∃s'. T(s, s') ∧ T(s', s'')` to the fixpoint; then `EF p = p ∨ ∃s'. R⁺(s, s') ∧ p(s')`, transferred back. `AG p = ¬EF ¬p`.
- **Soundness.** `◇X` in the engine is `∃i. constraint ∧ X(next(s, i))` — exactly the relation the closure is built from — so the closure's `EF` is the iteration's least fixpoint. `AG` is its dual under the same constrained diamond. Tests: forced squaring (threshold 0) agrees with pure iteration on the whole M2 nested-formula battery (closed, open, alternating, νμν, μνμ) and on a constrained design; on the raster both paths compute the same `EF p` and `AG EF p`.
- **Budget.** The second arena is 4 M nodes; the relation build and every squaring are refused at 80 % of it, and after 48 squarings. A refusal (a relation whose closure does not compress — a wide datapath, a multiplier) leaves the iteration exactly where it was: the rescue is attempted once per fixpoint, the refusal is remembered per model. `MUNUNU_BDD_SQUARING=0` disables it.

## What to update, per consumer

### monono

- **Expect `unknown` → definite on iteration-bound properties.** Any `--expect NAME=unknown` pin whose note read `abstained on the ITERATION budget` (raster / line-and-frame counters, wide down-counters, FSM-gated counters) may now fail because the property decides. Re-run and re-pin; a move `unknown → holds/violated` on this upgrade is this change.
- **A move `holds ↔ violated` is NOT this change** and is worth reporting with the design.
- **`decided_by` can change** from `native-bmc` / `cex` / b2 to `exact-symbolic` on the same value — the exact member now reaches a decision the portfolio used to get from a slower member.
- **`MUNUNU_BDD_ITER_BUDGET` raises you made for raster designs** (the CLAUDE.md table's "raise it pre-emptively for video/raster designs") are no longer needed for the `EF`/`AG` shapes; they still govern fixpoints the rescue does not take (game modalities, open bodies).

### ROSF

- No code change; verdict distribution shifts (fewer `unknown`), lane times fall on the affected designs.

### Report-parsing impact

None. Outcome vocabulary, `PropertyVerdict` fields and the JSON schemas are unchanged.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | verdict values: iteration-budget `unknown` → definite on the affected shapes; `decided_by` | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; same change on the SVA path | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The mechanism: equality with iteration, the budget abstention deciding, nested formulas, constraints.
cargo test -p mununu-core --lib -- squaring_

# 2. Your own design: does the rescue fire, and what did it cost? (debug line per evaluate; the
#    iteration count stops at the threshold when the closure takes over)
RUST_LOG=mununu_core::adapter::btor2::symbolic_bitblast=debug \
  mununu --quiet btor2 verify-recoverability design.btor2 --target "reg == v"

# 3. Re-run any corpus that pinned `unknown` with an `abstained on the ITERATION budget` note and
#    diff the outcome column. unknown -> definite is this change; holds <-> violated is not.
```

## Not covered here

- **Fixpoints the rescue does not take** — two-player (`ctrl`) modalities, guarded modalities, bodies mentioning an outer variable (`νZ. μY. (p ∨ (◇Y ∧ ◇Z))`-style) — iterate as before and keep their budgets.
- **The second arena's size** (4 M nodes) and the threshold (4,096) are constants, not flags; both were set from the measurements above and can be tuned with a measurement.
- **The remaining wall on the rescued shapes is manager creation** (two OxiDD arenas, ~1 s under the profile): B3 (a cone-sized arena) on the roadmap.
