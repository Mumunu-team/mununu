# Consumer briefing — 2026-10 OpenTitan SVA tranche 1: `--lift-override`, struct members and multi-dimensional elements, `$past` of outputs, three defect fixes

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates — and anyone validating the API against `docs/api-schemas/`.
>
> **Related:** [mununu#697](https://github.com/Mumunu-team/mununu/issues/697) (tracking), [#696](https://github.com/Mumunu-team/mununu/issues/696), [#699](https://github.com/Mumunu-team/mununu/issues/699), [#700](https://github.com/Mumunu-team/mununu/issues/700); [#698](https://github.com/Mumunu-team/mununu/issues/698) (found, not fixed). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change + a new flag / request field.**
>
> - `$past` (and `$stable` / `$changed` / `$rose` / `$fell`) of a combinational **output** or named net used to read the nearest register in its cone. A false HOLDS was possible. It now reads the net (#696).
> - A `--config-value` pin used in boolean position no longer makes the property skip on a parse error (#699).
> - A shift amount wider than 128 bits no longer crashes the run (#700, a regression from #695).
> - **New** CLI `--lift-override NAME=PATH` and API `lift_overrides: [{name, content}]`.
> - **Wider translator.** These now translate where they used to be `unsupported`: packed-struct members, elements of multi-dimensional packed arrays, `->` / `<->`, 1-bit `& | ^ ~^ ~`, constants slang folds, and bare sequence properties.

## What changed

- `adapter/btor2/shadow.rs` — the `__past` shadow source resolves to the named value exactly:
  - a state;
  - an input;
  - a wire alias of one state, still mirroring its init;
  - an output's driver;
  - a named net.

  A combinational source starts from zero at cycle 0, like an input's shadow.
- `adapter/slang/verify_auto.rs`:
  - `substitute_config_in_formula` writes `true` / `false` in boolean position;
  - `VerifyAutoOptions::lift_overrides` is applied in the lift only.
- `adapter/btor2/bit_blast.rs` — the shift-amount clamp no longer panics on wide amounts.
- `adapter/slang/translate.rs`:
  - `packed_sub` resolves struct members (first member most significant), multi-dimensional elements and part-selects as one slice;
  - `type_width` multiplies all packed dimensions. A reduction `&x` over a multi-dimensional array used to compare against the first dimension's all-ones;
  - embedded typedef references in struct types are expanded;
  - the new boolean operators, constants and bare sequences are lowered.
- `api/models.rs` — `SvVerifyAutoRequest.lift_overrides` (default empty). The request schema is regenerated.

## What to expect

On OpenTitan's own SVA with the real `prim_assert` macros, the census `e2e_opentitan_sva_census` covers 18 designs. It lifts with the real macros and falls back to the synthesis shim only when that lift fails.

| class | main `2a1710e` | this change |
|---|---|---|
| holds | 28 | 38 |
| holds, `$isunknown` fold only | 13 | 17 |
| violated | 6 | 12 |
| unknown | 0 | 2 |
| skipped (named reason) | 7 | 36 |
| unsupported (named reason) | 40 | 31 |
| designs that do not lift | 4 | 0 |

- **One decided verdict moved.** prim_packer_fifo `DataOStableWhenPending_A` went from VIOLATED to HOLDS. Its `$past(rvalid_o)` / `$stable(rdata_o)` read registers through the #696 defect. From the RTL, `rdata_o` cannot change while data is pending and unread. This is reasoned from the RTL, not replayed under Verilator.
- **Most new rows come from designs that now lift.** Many of them skip with a named reason: an output whose cone reaches a free input the lift cannot attribute, or an assertion-side signal absent from a shim lift.
- **New VIOLATED rows are model-only traces under unconstrained inputs.** The ibex and alert / escalation-sender checks are like this. Several of those designs rely on assumptions (`assume`) or a connected peer. Do not read them as RTL bugs.

## What to update, per consumer

- **ROSF** / **monono**:
  - Re-baseline `--expect-count`. Properties that were `unsupported` now translate.
  - Re-check any HOLDS on a property using `$past` / `$stable` / `$rose` / `$fell` of a combinational output. It was read through the nearest register.
  - Gates pinning `rst_ni` with `--config-value` may see skipped properties decide.
- **API clients that validate requests against the schema**: vendor the regenerated `sv-verify-auto-request.schema.json`. The field is optional.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator, shadow, simulator, new flag / field | **Yes** |
| `mununu-dev` | test image; schema drift tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e and the census run here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- past_of_a_combinational_output a_pinned_signal_in_boolean_position \
  simulator_shift_by_an_amount_wider lift_overrides_replace phase1_forms the_shipped_fragment_truth_table
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture \
  e2e_past_of_a_combinational_output e2e_phase1_forms_decide_with_twins
```

## Provenance

PR: (filled in once opened). Every `Repro: in-repo` reproducer failed on its parent commit with only its test hunks applied. That check was done by hand, because `make verify-repro` copies the whole test-bearing file and cannot separate an inline test from its fix.

## Not covered here

- The minted-expression compiler: arithmetic, `$past(expr)`, concatenation, shifts and dynamic selects. User-function inlining (`min` / `max`) and `assume` as preconditions. These are later tranches of the same plan.
- The exact-engine index-out-of-bounds panic on otbn / keymgr (#698). It is caught, so a verdict is lost, not the run.
