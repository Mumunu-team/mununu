# Consumer briefing — 2026-10 SVA `throughout` and `$countones` decide; slices of output ports fixed

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) on SystemVerilog assertions — ROSF's SVA lane, monono's `make formal`, and the `--expect-*` gates built on them.
>
> **Related:** tracking issue [mununu#691](https://github.com/Mumunu-team/mununu/issues/691); the earlier briefings [`2026-10-sva-fragment-widening.md`](2026-10-sva-fragment-widening.md) and [`2026-10-sva-ranged-antecedents.md`](2026-10-sva-ranged-antecedents.md); `docs/verifying-rtl.md` §3. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change, no shape change.** `e throughout S` over a bounded `S`, and `$countones(x)` in comparisons and in boolean position, used to come back in `unsupported`; they now translate and decide. One **defect fix** can change an existing verdict: a slice of an **output port** (`gnt[1:0]` where `gnt` is an output) was never minted and its property skipped; it now binds.

## What changed

| SVA | lowering |
|---|---|
| `a \|-> (e throughout (b ##[1:2] c))` | `b ##[1:2] c` with `e` at every cycle of the match — the start, the waited cycles and the match end |
| `$countones(x) <= 1`, `$countones(x[7:0]) == 2`, `$countones(x)` | a minted combinational count `x__popcount` (per bit: a 1-bit slice, a zero-extension, an adder), ≤ 64 bits |

**Defect fixed:** the lift keeps port names only on BTOR2 `output` lines, which the slice minting did not read, so a slice of an output port left its atom unbound — the property skipped with "unknown register/signal". Slices and counts of an output now resolve to the port's driver. `Repro: in-repo a_slice_of_an_output_port_is_minted_from_its_driver`.

**Still refused, with a reason:** `throughout` over an unbounded sequence or with a repetition inside its chain, a non-boolean invariant; `$countones` over more than 64 bits or over a non-signal; goto / `[=n]`, unbounded antecedents, PSL.

## What to expect

- `report.unsupported` shrinks; a newly-translated `$countones` assertion (one-hot / at-most-one checks) or `throughout` assertion may come back VIOLATED.
- A property slicing an output port that used to skip now decides.

## What to update, per consumer

- **ROSF** / **monono**: re-baseline `--expect-count`; re-check any lane that slices an output port.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator + minting change | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e runs here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- throughout xl7_countones augment_with_popcounts a_slice_of_an_output_port the_shipped_fragment_truth_table
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture e2e_throughout_and_countones_decide_with_twins
```

## Provenance

PR: [mununu#692](https://github.com/Mumunu-team/mununu/pull/692). Plan: SVA-extraction gaps, Part 1 steps 4–5. Semantic check for `throughout`: `throughout_lowering_agrees_with_a_brute_force_matcher` (exact engine vs a brute-force matcher on random periodic waveforms; fails on a deliberately wrong lowering).

## Not covered here

- Goto / non-consecutive repetition, unbounded antecedents and PSL — out of scope (plan step 6).
