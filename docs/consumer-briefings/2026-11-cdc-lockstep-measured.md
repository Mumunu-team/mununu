# Consumer briefing — the recorded CDC verdicts and the lockstep they rest on (2026-11)

> Source of truth: [`--merge-clock-domains`](../../crates/mununu-cli/src/main.rs#L2493) (CLI), [`merge_clock_domains`](../../crates/mununu-core/src/api/models.rs#L1727) (API), the `clock-domains-merged` note at [`verify_auto.rs`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L2426) — surface: CLI+API.

> **Audience:** monono (owns the three blocks and their recipes), rosf (drives corpora by their `[corpus] top`, where one of the findings bites), and anyone verifying a design with more than one clock.
>
> **Related:** roadmap A2/A3; [mununu#897](https://github.com/Mumunu-team/mununu/issues/897) (filed from this measurement); the 2026-10 "multi-clock refusal" briefing, whose refusal this measurement confirms.
>
> **TL;DR — no verdict changes, no rebuild. This is a measurement, written so the assumption behind nineteen recorded `holds` is stated once, in one place.** Every lift that produced those verdicts ran both clock domains in lockstep. That is by explicit opt-in: mununu refuses a multi-clock top by default and names the hazard; the recipes pass `--merge-clock-domains` and their comments say the domains tick 1:1; every merged run carries the `clock-domains-merged` note. The verdicts are correct **for that model**. A property that reads "whatever the far side does" was checked against the one schedule where a broken synchroniser looks fine, and the recorded rows say so. This briefing draws no soundness verdict; it names which rows rest on the assumption and what did not reproduce.

## What was measured (2026-11-08, `mununu-sva`, `mununu 0.5.10 (24bb5d7)`)

**The default refuses.** Without `--merge-clock-domains`, both multi-clock tops are refused before any engine runs: *"the model has one implicit clock, so lifting it would step every domain together and hide every crossing defect"*. `stage_cdc`: `clk_25mhz` 9 cells, `clk_mem` 6. `bundle_cdc`: `clk_dst` 4, `clk_src` 2.

**The recipes opt in, and say so.** The recorded recipes for `sprite_stage`, `bundle_cdc` and `pulse_cdc` all pass `--merge-clock-domains`; their comments read "the domains tick in LOCKSTEP (a 1:1 ratio, no phase)". In those lifts: every state cell has a `next` (the one next-less cell in `sprite_stage` is the recipe's `--cutpoint rd_data`); no `next` depends on a clock input; the clock nets are inputs read only through name aliases. Every merged run carries the `clock-domains-merged` note.

**The recorded rows reproduce, all by `exact-symbolic`:**

| block | recorded | on 24bb5d7 | twin |
|---|---|---|---|
| `sprite_stage` (guarantees at `sprite_stage.sv:565`, `:569`) | 8/8 hold | 8/8 hold | `sva_0`, `sva_1`, `ann_guarantee_0` violated; `ann_guarantee_1` holds |
| `bundle_cdc` (guarantees at `:103`, `:104`) | 7/7 hold | 7/7 hold | `sva_2` violated |
| `pulse_cdc` | 4/4 hold | 4/4 hold | `sva_0` violated |

Those **nineteen `holds` rest on the 1:1 assumption**. Nothing here says they are wrong; it says what they are verdicts about.

## What did not reproduce

`stage_cdc` verified the way `rtl/bringup/stage_cdc/rosf.toml` lists it (`top = "stage_cdc"`, sources `stage_cdc.sv`, `sprite_stage.sv`, `sdram_pll.sv`) does not reproduce the `sprite_stage` rows, for two independent reasons:

1. **`EHXPLLL` has no definition in that source list**; the stub in the consumer's tree is Verilator-only. The measurement used a port-only `(* mununu_missing_module *)` stub, kept with the report, not in the consumer's tree. *Consumer item.*
2. **A guarantee written inside `sprite_stage` names `fill0_q`; under the `stage_cdc` top the lift carries it as `u_stage.fill0_q`, the atom does not bind, and both rows are `skipped` ("predicate references unknown register/signal `fill0_q`").** The row is reported, not silent; the gap is that an annotation written in a reused block stops applying the moment the block is verified in context. *mununu item: #897.* Until it is fixed, a corpus driven by a parent top verifies none of the annotations its submodules carry; the recorded `sprite_stage` rows were produced with `sprite_stage` as its own top, which is why they exist.

## One note for the clock census (roadmap B1)

The consumer's `make cdc-check` analyses every corpus except `cpu_tests` (it cannot find a generated `tests.hex`). For `stage_cdc` it reports two clocks and the `clk_25mhz → clk_mem` crossing plus its reset crossing. It names a clock by the first net name on the `CLK` bit and never traces roots, so it cannot tell a primary input from a PLL- or logic-derived clock; from source, `clk_25mhz` is a top input (`stage_cdc.sv:25`) and `clk_mem` is `sdram_pll`'s `clk_sys` (`:33-35`): a real two-domain pair, not a gated clock.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **monono** | The nineteen `holds` on the three blocks are verdicts under the lockstep model your recipes opt into and document. The twins' violations show the properties are not vacuous under that model. | None required. If a tier-3 claim is meant to range over the far side's timing, that claim is not what these rows establish; say so where the claim is recorded, or verify it another way. Your `EHXPLLL` stub does not elaborate under slang. |
| **rosf** | A corpus driven by a parent top (`[corpus] top`) loses every annotation its submodules carry until #897 is fixed; the rows come back `skipped` with the reason, not green. | Read `skipped` rows' reasons in gates that run by parent top. No flag or route change. |
| **mununu-ui** | Nothing; no surface change. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| all `mununu/docker/Dockerfile*` | No. | No engine, flag, schema or route change; this briefing records a measurement. |
| rosf `docker/Dockerfile*` | No. | Same. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

- Roadmap items A2 (one lift settles it) and A3 (whatever it returns goes out as a briefing), measured on the second machine on 2026-11-08; the full report, both BTOR2 dumps per block and the `cdc-check` output are in the private e2e history (`a2-cdc-lockstep`).
- A2's original subjects (`spr_desc_cdc`, `spr_cost`) no longer exist in the consumer's tree; the measurement was re-targeted at the two-domain blocks that remain.

## Not covered here

- Whether a per-domain clock model (the roadmap's B4–B6) is built at all depends on the clock census across the consumer's remaining eleven multi-clock files, not on this one; this briefing settles only that the recorded rows are lockstep verdicts and are labelled as such.
- #897's fix.
