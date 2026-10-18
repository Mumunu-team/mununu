# Consumer briefing — 2026-10 full-IP SVA: hierarchical references into any instance, members of struct-array elements, atom-less assertions decided, `genblk` dropped from model names

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates — and anyone reading signal names from formulas, counterexamples or `MUNUNU_SHADOW_BTOR2_DUMP`.
>
> **Related:** [mununu#722](https://github.com/Mumunu-team/mununu/issues/722); follows [2026-10-sub-instance-sva-and-safety-traces](2026-10-sub-instance-sva-and-safety-traces.md) (#709) and [2026-10-for-generate-names](2026-10-for-generate-names.md) (#713). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **more decided properties and one new `decided_by` value — re-baseline required.** Found by running a full OpenTitan IP (`edn`: TL-UL register block + core + 8 endpoint FSMs, 184 registers) where 0 of 52 assertions decided. (1) A hierarchical reference into a module instantiated more than once now resolves to its instance, written directly or through a macro (OpenTitan's `ASSERT_PRIM_*_ERROR_TRIGGER_ALERT`). (2) A member of an element of a packed array of structs (`edn_o[i].edn_ack`, `alert_tx_o[1].alert_p`) translates. (3) An assertion whose formula has no atom (`ASSERT_KNOWN` once `$isunknown` folds) is decided before any engine, `decided_by: "constant"`, instead of skipping over the bit cap on a wide design. (4) The lift's `genblkN.` segments (sv2v's anonymous `else` block of an `else if` chain) are dropped from model symbol names, so they match the SystemVerilog hierarchical names.

## What changed

- `adapter/slang/mod.rs::run_ast_json_with_source`: extraction preprocesses the unit with `slang -E` and dumps the AST from that text with `--ast-json-source-info`; `translate.rs::resolve_hier_path` reads a `HierarchicalValue`'s path from the span and walks the instance tree (children, generate blocks, `gen[i]` through the enclosing genvar, `$root.`), finding the declaration by name in the instance reached. slang names such a reference by a per-instance symbol it serialises for the first instance only, so the address alone can never name a later instance.
- `translate.rs::packed_sub` / `type_width` / `index_positions`: `struct packed{…}name[hi:lo]` is an array of structs — elements are the structs, a member of an element is a slice of the right element; the whole array's width counts every element.
- `mu_calculus::Formula::constant_value` + `verify_auto`: an atom-less formula is decided on the one-state total model (`[] φ` = `<> φ` = `φ`; a fixpoint settles in two steps) before the engines; `decided_by: "constant"`.
- `btor2/parser.rs::flat_symbol_names`: `genblk<N>.` segments are dropped from a symbol when the result stays unique among the text's symbols.

## What to expect

- Properties that were `unsupported` ("instantiated N times", "its type `logic[0:0]` is not a packed struct") now decide; properties that skipped with "design has N register+input bits (> 192) after cone-of-influence" on an `ASSERT_KNOWN` now HOLD with `decided_by: "constant"` (a `two-state-fold` note still marks the fold).
- `decided_by` gains the value `"constant"`. Aggregations over engines should accept it.
- Model symbols lose `genblkN.` segments: a formula, counterexample or dump that showed `u_fifo.genblk1.gen_normal_fifo.u_cnt.err_q` now shows `u_fifo.gen_normal_fifo.u_cnt.err_q`. A `--predicate`, `--cutpoint`, `--config-value` or `sv mutate` register name that spelled the `genblk` segment must drop it.
- No verdict of a previously decided property changes.

## What to update, per consumer

- **ROSF** / **monono**: re-baseline `--expect-count` and unsupported counts; accept `"constant"` in `decided_by`; drop `genblkN.` from any pinned register name.
- **mununu-ui**: nothing required.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | extraction, translator, model naming, verdict pass | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e runs here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change (slang `-E` and `--ast-json-source-info` exist in the pinned 11.0.448).

## Test the transition

```bash
cargo test -p mununu-core --lib -- hierarchy_refs_resolve_from_real_slang_json a_member_of_an_element_of_a_packed_array_of_structs an_atom_less_formula_is_decided x722_genblk
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture e2e_hierarchical_references_name_their_instance e2e_an_atom_less_assertion_decides_without_a_cone
```

## Provenance

PR: pending — branch `feat/hier-refs-any-instance`. `Repro: in-repo` — `hierarchy_refs_resolve_from_real_slang_json` (main refuses all five references), `a_member_of_an_element_of_a_packed_array_of_structs_is_its_slice` (main: `logic[0:0]`), `an_atom_less_formula_is_decided_without_a_model`, `x722_genblk_segments_are_dropped_unless_they_collide`; e2e `e2e_hierarchical_references_name_their_instance`, `e2e_an_atom_less_assertion_decides_without_a_cone`. Measured on OpenTitan `edn` (558921c, 58 files, 200 registers; `examples/verify/ip_opentitan_edn`, 2026-10-18): the IP's 52 assertions went from 0 decided / 39 unsupported on main to 23 HOLDS (13 `ASSERT_KNOWN` by `constant`, 10 error → alert checks by the `explicit` cube — the main FSM, all 8 endpoint FSMs, the register-write one-hot) / 1 VIOLATED (`EdnFatalAlertNoRsp_A`, 6 cycles from reset, trace attached) / 20 ⊥ / 8 unsupported (`[->1]`); `pattgen` 14 of 14 HOLDS; `csrng` 20 HOLDS / 14 ⊥ / 3 unsupported behind the black-boxed cipher core. Ledger §3.3b-6.

## Not covered here

- A hierarchical reference whose index is not a constant where it is written (refused with the reason).
- Unnamed generate blocks that legitimately carry a register referenced as `genblkN.x` in SVA: the segment is dropped on the model side only when unique; such a reference still binds by the stripped name.
