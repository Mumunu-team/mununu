# Consumer briefing — 2026-10 OpenTitan SVA tranche 2: the expression compiler, function inlining, comparison conversions applied

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates.
>
> **Related:** [mununu#702](https://github.com/Mumunu-team/mununu/issues/702) (defect); plan "cover the OpenTitan assertions", Phases 2 + 3; tranche 1 was [mununu#701](https://github.com/Mumunu-team/mununu/pull/701). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change. There are no new flags or JSON fields.**
>
> - **A defect fix (#702).** A comparison used to drop slang's conversions on its operands: a sign extension on a signal (`r == 8'(s)`) or a narrowing cast on a literal (`x4 != 4'(20)`). Either could report a **false HOLDS**. Such comparisons are now lowered with the conversions applied.
> - **Many `unsupported` assertions now decide.** These are expressions over arithmetic, shifts, `?:`, concatenation, signed comparisons, `$past` of an expression, reductions, `$countones` / `$onehot` of an expression and in-range dynamic selects, plus calls to single-`return` user functions such as OpenTitan's `min` / `max`.
> - **How it works.** Each such expression becomes one minted combinational node, `mununu_x_<hash>`, in the model. Property formulas in the report may now name it.

## What changed

- `adapter/slang/expr_ir.rs` (new):
  - builds a width-checked expression from slang's per-node types and `Conversion`s, refusing any width it would have to guess;
  - pushes `$past` down to the signals;
  - inlines single-`return` functions.
- `adapter/btor2/minted_expr.rs` (new) — the expression IR, its reference evaluator, and the BTOR2 emission (`augment_with_expressions`). The emission runs after the shadow, slice and count passes in `verify_auto`.
- `adapter/slang/translate.rs`:
  - `bool_expr` falls back to the compiler where the atom lowering refuses;
  - a value-changing conversion on a comparison operand sends the comparison to the compiler (#702);
  - literals are read after their conversions;
  - folded parameters keep their type;
  - `-4'sd8` parses as negative.
- **Still refused, with a reason:**
  - `/ % **`, and a multiplication wider than 32 bits;
  - a dynamic index that can leave the range (SV reads X; the model has no X);
  - X/Z constants;
  - `>>>` of a signed operand in an unsigned context, where the standard and the simulators disagree;
  - unpacked-array content;
  - function bodies other than one `return`.

## What to expect

On the OpenTitan census (`e2e_opentitan_sva_census`, OpenTitan's own SVA on 18 designs):

| class | main `881dc70` | this change |
|---|---|---|
| holds | 38 | 51 |
| holds, `$isunknown` fold only | 17 | 17 |
| violated | 12 | 15 |
| unknown | 2 | 2 |
| skipped (named) | 36 | 43 |
| unsupported (named) | 31 | 8 |

- **No decided verdict moved.**
- **New HOLDS:** all of prim_count's arithmetic counter checks, including the `min` / `max` ones, and both arbiters' `RoundRobin_A` and `IndexIsCorrect_A`.
- **New VIOLATED, model-only traces:**
  - the arbiters' `ReqStaysHighUntilGranted0_M` is an `assume` that verify-auto still checks as an assertion;
  - ibex's exception-priority one-hot check runs over unconstrained inputs.

## What to update, per consumer

- **ROSF** / **monono**:
  - Re-baseline `--expect-count`, because properties that were `unsupported` now decide.
  - Re-check any HOLDS on a comparison between differently-sized signed operands, or against a cast literal (#702).
  - A formula naming `mununu_x_…` is expected. The node is visible in `MUNUNU_SHADOW_BTOR2_DUMP`.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator + new model augmentation | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e and the census run here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- minted_expr expression_forms_translate a_sign_extended_comparison \
  a_narrowing_cast the_shipped_fragment_truth_table
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture \
  e2e_minted_expressions_agree_with_yosys_elaboration e2e_prim_count_saturation_checks_decide_with_twins
```

## Provenance

PR: [704](https://github.com/Mumunu-team/mununu/pull/704). `Repro: in-repo` for #702. Both reproducers fail on main `881dc70`, producing `(r == s)` and `(x4 != 20)`. This was checked by hand in a worktree. The defect was found by the yosys-elaboration oracle.

## Not covered here

- Phase 4, `assume` statements as preconditions, is the next tranche.
- Named property instances (`AssertionInstance`), unpacked-array reads, and the alert sender's sequence antecedents remain unsupported.
- A property that reads a signal inside a sub-instance (`err_d` of an inner `prim_count`) skips with "unknown register/signal". A minted expression over such a signal skips under its `mununu_x_…` name.
