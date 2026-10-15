# Consumer briefing — 2026-10 SVA fragment widening (A): bit-selects, `$isunknown`, liveness SVA; slices of non-zero-based vectors fixed

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) on SystemVerilog assertions — ROSF's SVA lane, monono's `make formal`, and the `--expect-*` CI gates built on either.
>
> **Related:** tracking issue [mununu#685](https://github.com/Mumunu-team/mununu/issues/685); the executable fragment table `the_shipped_fragment_truth_table` (`crates/mununu-core/src/adapter/slang/translate.rs`); `docs/verifying-rtl.md` §3 "Coverage is a fragment". Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change.** Assertions that came back in `unsupported` now translate and decide: constant bit-selects (`sig[k]`), history calls over a bit or slice (`$rose(sig[3])`, `$stable(sig[7:4])`), `$isunknown(x)` (folded to `false`, with a note), and liveness SVA (`s_eventually`, `##[m:$]`, `[*n:$]`, `nexttime`, `always`). Two **defect fixes** can change an existing verdict: a slice of a vector not declared `[N-1:0]` now reads the right bits, and `$past(sig[hi:lo])` now builds the shadow it needs. Two new per-property note kinds: `two-state-fold`, `strong-sequence-reading`. No JSON-shape change, no new flag or route.

## What changed

**Newly decided (were `unsupported`):**

| SVA | lowering | note |
|---|---|---|
| `sig[k]` (packed vector, typedef, packed struct) | the 1-bit slice, bare in boolean position | — |
| `$rose` / `$fell` (one bit), `$stable` / `$changed` over `sig[k]` or `sig[hi:lo]` | slice now vs the same slice of `sig__past` | — |
| `$isunknown(x)` | `false` (the model is 2-state) | `two-state-fold` |
| `s_eventually b`, `a \|-> s_eventually b` | `AG AF b`, `AG(a → AF b)` | — |
| `a \|-> ##[m:$] b` | `AG(a → AXᵐ AF b)`, read strongly | `strong-sequence-reading` (not for `strong(…)`) |
| `a \|-> b[*n:$]`, n ≥ 1 | `b[*n]` | — |
| `s_eventually [m:n] b`, `eventually [m:n] b`, `nexttime p`, `always p` | bounded window, `AX p`, `AG p` | — |

**Defects fixed (verdicts may change):**

1. **Slices of non-zero-based vectors read the wrong bits.** yosys numbers bits from 0 at the right-most declared index; the slice path used SV indices as bit positions. `x[11:8]` of `logic [15:8] x` asked for bits 11..8 of an 8-bit vector — the slice was never minted and the property **skipped**; on `logic [11:4] x`, `x[7:4]` read SV bits 11..8 **silently**. Zero-based `[N-1:0]` vectors (the usual case) are unaffected. `Repro: in-repo slice_of_an_offset_vector_reads_the_yosys_bit_positions`.
2. **`$past(sig[hi:lo])` never requested `sig`'s shadow**, so its slice had no base and the property skipped. `Repro: in-repo past_over_a_slice_requests_the_base_shadow`.

**Strong reading of unbounded sequences.** IEEE 1800-2017 makes a sequence in an `assert` weak by default; a weak `a |-> ##[1:$] b` holds on every infinite trace. mununu reads it as `a |-> s_eventually b` and attaches `strong-sequence-reading` to the property. An explicit `weak(##[1:$] b)` is refused (it checks nothing).

**Still refused, with a reason:** ranged and unbounded antecedents, `##[m:$]` before anything but the last element, goto / `[=n]`, `throughout`, `$countones`, ranged `nexttime`/`always`, dynamic and multi-dimensional selects, enum-typed bases, `cover` of an eventuality, PSL.

## What to expect

- `report.unsupported` shrinks; `coverage-summary` counts move from "untranslatable" to definite / ⊥.
- A design whose `unsupported` list carried liveness assertions now runs them through the exact engine (νμ), and past its reach — or past a primary-input atom, which the exact engine refuses — through the liveness-to-safety rescue: `AG(a → AF b)`, `AG AF b`, and `AG(a → AXᵐ AF b)` (`|=>`, `##[m:$]`, through an m-latch request delay line), with the request and grant allowed to be boolean combinations (`(req && st == IDLE) |-> s_eventually done`). An eventuality over a relational atom stays ⊥ past the exact engine, with a reason.
- **The `verify-liveness` CLI/API verb is unchanged** (single-atom `--request` / `--grant`); only `sv verify-auto`'s ⊥ escalation gained the compound and delayed forms.
- `!$isunknown(x)` HOLDS trivially, with `two-state-fold` against that property. It is **not** an X-propagation check.

## What to update, per consumer

- **ROSF** (SVA lane): an `--expect-count` / `--expect-all-hold` gate may now see more properties, and a liveness property may come back VIOLATED where it used to be unsupported. Re-baseline the counts; read the two new note kinds if you surface notes. If any lane slices a vector declared other than `[N-1:0]`, re-run it: those properties were skipping (or, for `[hi:lo]` with `lo > 0` and `hi < width`, reading the wrong bits).
- **monono** (`make formal`): `!$isunknown` assertions, if any, will HOLD with the fold note — do not count them as X-checks. Liveness assertions written as `##[1:$]` will carry `strong-sequence-reading`; spell them `s_eventually` to state the intent.
- Anyone with `--expect-*` gates: the counts change; verdicts of previously-translated properties change only through the two defect fixes above.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator + rescue changes | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e tests run here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
# The fragment table, the reproducers and the translator → rescue contract (no tools):
cargo test -p mununu-core --lib adapter::slang::translate adapter::liveness_rescue \
  every_translator_atom_shape_has_a_known_seeding_classification
# End to end, in the mununu-sva image:
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture \
  e2e_sva_liveness_forms_decide_with_responder_and_staller_twins \
  e2e_bit_select_offset_slice_edge_and_two_state_fold_decide
```

## Provenance

Fix + feature PR: [mununu#686](https://github.com/Mumunu-team/mununu/pull/686). Plan: SVA-extraction gaps, Part 1 steps 1–2 (2026-10-14). Shapes captured from slang 11.0.448 in `crates/mununu-core/src/adapter/slang/testdata/sva_{fragment,bit_select}_forms.*`.

## Not covered here

- The second widening (ranged antecedents, `throughout`, `$countones`) ships separately with its own briefing.
- An eventuality over a relational atom (`s_eventually (x == y)`) through the liveness-to-safety rescue (a recorded gap: the monitor binds single register comparisons).
- `we |=> (q == $past(d[7:0]))` (a register-vs-register consequent behind `|=>`) now binds but is ⊥: the safety-rescue lane's existing leaf-shape gap, unchanged here.
