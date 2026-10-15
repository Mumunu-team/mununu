# Consumer briefing — 2026-10 SVA ranged antecedents: `(a ##[m:n] b) |-> c` and `a[*m:n] |-> c` now decide

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) on SystemVerilog assertions — ROSF's SVA lane, monono's `make formal`, and the `--expect-*` gates built on them.
>
> **Related:** tracking issue [mununu#687](https://github.com/Mumunu-team/mununu/issues/687); the first widening's briefing [`2026-10-sva-fragment-widening.md`](2026-10-sva-fragment-widening.md); `docs/verifying-rtl.md` §3. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change, no shape change.** Assertions whose antecedent has a range delay (`(a ##[1:2] b) |-> c`), a range repetition (`a[*1:3] |-> c`) or a repetition inside a chain (`a[*2] ##1 b |-> c`) used to come back in `unsupported`; they now translate and decide. No verdict of a previously-translated assertion changes (the fixed-delay lowering is byte-identical).

## What changed

IEEE 1800-2017 §16.12.7: the consequent must hold at **every** match end of the antecedent. The translator lowers each range to a conjunction over its alternatives, in negation-normal form:

| SVA | mu-calculus |
|---|---|
| `(a ##[1:2] b) \|-> c` | `!a \|\| [] ((!b \|\| c) && [] (!b \|\| c))` |
| `a[*1:3] \|-> c` | `!a \|\| (c && [] (!a \|\| (c && [] (!a \|\| c))))` |
| `a[*2] ##1 b \|-> c` | `!a \|\| [] (!a \|\| [] (!b \|\| c))` |

Guards: at most 64 match-end alternatives (the product over the links) and 64 cycles of span; past either, the assertion is refused with the count. Unbounded antecedents (`##[m:$]`, `[*n:$]` before `|->`), `[*0…]`, goto / `[=n]` and nested sub-sequences stay refused with a reason.

## What to expect

- `report.unsupported` shrinks for designs with variable-latency handshake assertions.
- Under the default engine (`portfolio-sequential`) these decide through the exact engine — VIOLATED with a witness when a later match end fails. Under `--engine explicit` (the predicate cube alone) a HOLDS still decides but a violation at a nested match end can stay ⊥; that is sound, and the reason is reported.

## What to update, per consumer

- **ROSF** / **monono**: re-baseline `--expect-count`; a newly-translated assertion may come back VIOLATED. Lanes pinned to `--engine explicit` should expect ⊥ rather than VIOLATED on such a violation and may want the default engine for these properties.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator change | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e runs here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- xl5_range ranged_antecedent xl5_repetition_inside the_shipped_fragment_truth_table
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture e2e_range_antecedent_decides_on_handshake_fsm
```

## Provenance

PR: [mununu#688](https://github.com/Mumunu-team/mununu/pull/688). Plan: SVA-extraction gaps, Part 1 step 3. Semantic check: `ranged_antecedent_lowering_agrees_with_a_brute_force_matcher` (exact engine vs a brute-force SVA matcher on random periodic waveforms; fails on a deliberately wrong lowering).

## Not covered here

- `throughout` and `$countones` (still planned).
- A rescue lane for nested-box safety violations under `--engine explicit`.
