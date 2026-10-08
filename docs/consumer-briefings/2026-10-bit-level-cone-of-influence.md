# Consumer briefing — 2026-10 the exact engine's cone of influence is bit-level: a field of a crossed vector no longer inherits the vector

> **Audience:** monono first — this is your [#602](https://github.com/Mumunu-team/mununu/issues/602) (`rtl/vpu/vpu_status`, plan step R2b). Also ROSF and any consumer whose `sv verify-auto` reports `skip-over-cap-reason` on a property about one field of a wide packed record.
>
> **Related:** closes [mununu#602](https://github.com/Mumunu-team/mununu/issues/602)'s minimum ask (constant part-selects); the address-mux shape is measured here and tracked as its own lever. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change (more properties decide; none moves).** The exact engine's cone of influence is now computed **per bit** and every partially-read register is narrowed to its kept bits before the bit-blast. A 32-bit field read through a constant part-select of a 23-word (736-bit) vector crossed twice is a **96-bit** cone, not 2,208, and the exact engine decides it instead of skipping on the cap — a 2-valued verdict with a witness where the abstracting engines returned ⊥. Verdicts that already decided are identical (the wall-class matrix is byte-identical at both granularities; `MUNUNU_COI=signal` is the differential switch). **A field read through an address mux is not rescued by this** — the mux reaches every word structurally (measured: 2,213 bits on the same vector) — and that is the half of your `vpu_status` property that still needs a different lever.

## What changed

- `dep_graph::bit_cone` — the property's cone at bit granularity: for every leaf cell the bits the property can depend on, closed under `state → next` bit by bit. Pure re-maps (`slice`, `concat`, extension, bitwise, `not`) map exactly; a carry chain (`add` / `sub` / `inc` / `dec` / `neg`) makes bit `i` depend on bits `≤ i`; a comparison, reduction, multiplication, division, overflow predicate or a shift by a **symbolic** amount depends on every operand bit; a shift by a **constant** is re-mapped by that amount; `ite` makes every output bit depend on the whole condition. A `constraint` / `fair` / `justice` that shares a bit with the cone joins it whole. A cone that reaches an array stays signal-level.
- `bit_blast::narrow_leaves` — every leaf with a partial mask becomes a narrow leaf of its kept bits, under the **same NID and name** (so atoms, `init` / `next` lines and a two-player `controllable` match are untouched), with a reconstruction of the full width (zeros outside the kept runs) that every reader is redirected to. `next` is the kept bits of the old next value; a constant `init` is narrowed the same way.
- The over-cap diagnostic (`skip-over-cap-reason`) counts the same bits the engine bit-blasts, so it no longer reports a property over the cap that the engine just decided.
- `MUNUNU_COI=signal` restores the whole-register cone.

**Soundness.** An out-of-cone bit cannot influence any in-cone bit's next value nor any atom (the cone over-approximates the bit-blasted semantics and is closed under `next`), so reading it as `0` changes nothing observable — which is exactly what the engine already did to a whole out-of-cone register, now one bit at a time. The wall-class matrix's twelve cases are identical at both granularities; the new `crossed_vector_field` case decides only at bit level (HOLDS) and is ⊥ on every lever at signal level.

## What it does for `vpu_status`, honestly

Your SVA reads the merged word through the generated 24-arm mux:

```
(addr == 20'h00004) |-> (rdata == status_word)
```

| read shape | signal-level cone | bit-level cone | exact engine |
|---|---|---|---|
| `words_x[159:128]` — a constant part-select | 3 × 736 = 2,208 bits | **96** | decides |
| `rdata` — the 23-arm address mux | 2,208 | **2,213** | still over the cap |

The mux is the obstacle, not the cone: bit `i` of `rdata` depends on bit `i` of **every** arm, so no structural cone can see that `addr == 4` selects one. What would decide it is constant-propagating the antecedent's `addr` through the consequent's cone (`AG(addr == K → φ(addr))` ≡ `AG(addr == K → φ(K))` for a same-cycle `|->`), after which the bit cone is the 96 above. That is a separate lever, tracked as a follow-up issue with these numbers. **Until it lands, phrase the property on the field**: a property over `status_x[2:1]` (or any constant slice) decides today; the two contrast twins you could not refute (merge omitted, flags swapped) are exactly the ones a slice-phrased property on the exact engine refutes.

## What to update, per consumer

### monono

- Re-run the `sv verify-auto` lanes with `skip-over-cap-reason` notes. Properties phrased on a slice of a crossed record decide now; the `decided-by` field says `exact-symbolic` where they moved from `explicit`. Expect no verdict to flip — a property the explicit engine decided keeps its verdict, now corroborated by a 2-valued engine.
- `verify.sh` pins that read `expect_named … HOLDS` from the explicit engine will see `decided_by: "exact-symbolic"`; a gate pinning the engine, not the verdict, needs updating.
- For `vpu_status` specifically: see the table above — the mux form still skips.

### ROSF and others

- No wire-shape change. More `holds` / `violated`, fewer `skipped` with `skip-over-cap-reason`, on designs with wide packed records read by part-select.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | exact-engine admission; more properties decide | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The rules, the rewrite, the crossed-vector shape at both granularities:
cargo test -p mununu-core --lib --features api -- x602_
# 2. Verdict identity on the fixed class set, both ways:
cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture
MUNUNU_COI=signal cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture   # fails only on the new case: "decided by no lever"
# 3. Your design: diff `sv verify-auto --json` with and without MUNUNU_COI=signal — only
#    `skipped` → decided moves are expected; any decided → decided change is worth reporting.
```

## Not covered here

- Antecedent constant propagation through a mux (the `vpu_status` form) — the follow-up issue.
- Arithmetic is kept conservative (`add` keeps bits `≤ i`; `mul` and a symbolic shift keep everything); a finer carry analysis would shrink some counter cones but was not needed for the class measured.
- The cube engines are unchanged; this is the exact engine's admission only.
