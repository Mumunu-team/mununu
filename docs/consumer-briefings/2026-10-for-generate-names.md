# Consumer briefing — 2026-10 assertions in `for` generates are checked; iterations are spelled `gen$k`

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates — and anyone reading formulas, counterexamples or `MUNUNU_SHADOW_BTOR2_DUMP` from it.
>
> **Related:** [mununu#713](https://github.com/Mumunu-team/mununu/issues/713); follows [2026-10-sub-instance-sva-and-safety-traces](2026-10-sub-instance-sva-and-safety-traces.md) (mununu#709). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **more decided properties — re-baseline required.** An assertion that reads a signal of a `for`-generate iteration (declared inside the loop, or in an instance the loop elaborates — OpenTitan's `csrng_cmd_stage`, `prim_alert_sender`, …) used to come back `unsupported` ("bracketed hierarchical name"). It is now checked once per iteration. The iteration `gen[k]` is written **`gen$k`** wherever a signal name appears in a formula, a counterexample or the dumped model.

## What changed

- `btor2/parser.rs::flat_name` / `flat_symbol_names`: every constant index `[k]` in a name is written `$k`; applied to the lifted model's symbols (`slang/verify_auto.rs::lift_sv`) and to the translator's resolved names (`slang/translate.rs::scope_refs`). An index that cannot be read stays refused.
- A port connected to `a[j]` inside a loop reads the iteration's own bit (the genvar is folded before the connection is substituted).
- `sv mutate`: a register named with brackets (as `--list` prints it) is matched against the `$` spelling.

## What to expect

- Fewer `unsupported` rows on designs with asserting modules inside `for` generates; each becomes one property per iteration. Property `name`s keep the brackets (`g_sub[0].u_s.fg_sub_sva_4`); `label`s are unchanged and repeat once per iteration.
- Formulas, counterexample state names and `MUNUNU_SHADOW_BTOR2_DUMP` symbols spell iterations `g_ch$1.r` — including names that had brackets for other reasons (a yosys-split wire `data[3]` is `data$3` in the dump).
- No verdict on a property that was already decided changes.

## What to update, per consumer

- **ROSF** / **monono**: re-baseline `--expect-count` / unsupported counts; a parser that matched bracketed names in formulas or counterexamples should match `$k`.
- **mununu-ui**: nothing required.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator + model naming | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e runs here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- x713_ hierarchy_forms_translate_from_real_slang_json
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture e2e_assertions_inside_a_for_generate
```

## Provenance

PR: pending — branch `feat/for-generate-names`. `Repro: in-repo` — `hierarchy_forms_translate_from_real_slang_json` and `e2e_assertions_inside_a_for_generate_are_checked_per_iteration` (main 69b92a0 refuses the loop assertions).

## Not covered here

- A hierarchical reference into a module instantiated more than once (refused: slang names the declaration, not the instance).
- Indexed part-selects `x[i +: w]` (refused, mununu#565).
