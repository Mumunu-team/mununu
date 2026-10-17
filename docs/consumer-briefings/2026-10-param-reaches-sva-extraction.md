# Consumer briefing — 2026-10 `--param` reaches the SVA extraction: bound checkers decide again, parameters fold to their override, and an override that cannot reach the SVA is refused

> **Audience:** anyone running `sv verify-auto` with `--param` (CLI) or `params` (`POST /api/v1/sv/verify-auto`) — monono first (pinned at `5d220c0` because of this), then any consumer whose SVA references a parameter.
>
> **Provenance:** [mununu#717](https://github.com/Mumunu-team/mununu/issues/717), measured by monono. Code: `adapter/slang/mod.rs` (`ast_json_args`, `run_ast_json_with_params`), `adapter/slang/extract.rs` (`extract_sva_with_params`), `adapter/slang/verify_auto.rs` (`extract_front`, `prepare_model`), `adapter/slang/translate.rs` (`parameter_values`), `adapter/btor2/minted_expr.rs` (`augment_with_expressions_explained`). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

The SVA was extracted by a slang run that never received `--param`, so it described the design at its parameter **defaults** while the lift checked the overridden design. Since mununu#712 (bound ports read their connection) this skipped every bound property whose port is sized for the narrowed width. Since mununu#632 it could also fold a parameter in an atom to its default and return a **wrong verdict**. The extraction now elaborates under the same overrides as the lift.

**Verdicts change.** Properties that were `skipped` under `--param` decide again. Properties that compare against an overridden parameter now use the override, and can flip in either direction relative to the default-folded verdict. A `MODULE.NAME` override that slang cannot apply is now an error instead of a silent default.

## What changed

| Situation | Before | After |
|---|---|---|
| bound SVA, port sized for a `--param`-narrowed width (monono's `affine_cost`, `spr_scan`, `spr_stat`, `sprite_eval`) | `skipped — … unknown register/signal mununu_x_…` | decided |
| an atom comparing against a parameter that `--param` overrides | folded to the **default** value | folded to the override |
| a generate branch selected by an overridden parameter | extracted for the default branch | extracted for the override's branch |
| `--param MODULE.NAME=V` where `MODULE` is not a top-level module in slang | lift applied the override, SVA used the default | refused: "the override did not reach the SVA extraction — slang elaborated `NAME` as …" |
| an expression that still cannot be built | the property skipped naming a hash | additionally an `expression-leaf-unresolved` scope note on the property, naming the leaf and both widths |

## What to update, per consumer

- **monono** — re-run the full formal lane at the fix commit and unpin from `5d220c0`. Expected: the 30 decided → skipped verdicts in the four `--param` blocks decide again, and the three contrast twins among them are VIOLATED again. Any verdict that differs from `5d220c0` beyond that is worth a look: a property comparing against an overridden parameter was judged at the default before.
- **Any `--param MODULE.NAME=…` user** — if the run is now refused, the SVA was previously being judged at the default for that module. Override the parameter on the top module, or wrap the submodule as the top.
- **Parsers of `notes`** — one new `kind`, `expression-leaf-unresolved`, level `scope-caveat`, with `property` set. The JSON shape is otherwise unchanged.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | verdicts under `--param`, a new refusal, a new note kind | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; where `sv verify-auto` runs | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles mununu; affected only where it passes `--param` | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# in the mununu-sva image
cargo test -p mununu-core --lib --all-features -- --ignored e2e_717
cargo test -p mununu-core --lib -- ast_json_args_pass collect_parameter_values an_unresolved_leaf_is_explained
```

## Not covered here

- `--top` is still not passed to the SVA extraction, so uninstantiated checker modules keep being elaborated. slang's `-G` applies to **every** top-level module declaring the name; the lift's `chparam` targets `--top`. A second top-level module declaring a parameter of the same name now also takes the override on the SVA side.
- Only an integer override is checked for having reached the extraction; a string or type parameter is not folded into atoms, so it cannot fold to a wrong value.
