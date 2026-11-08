# Consumer briefing — a `@mununu_guarantee` inside a submodule is checked in every instance under the top (mununu#897, 2026-11)

> Source of truth: [`expand_annotation_instances`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs), [`TranslationReport::instance_prefixes`](../../crates/mununu-core/src/adapter/slang/translate.rs), [`MununuAnnotation::module`](../../crates/mununu-core/src/mununu_annotations/mod.rs) — surface: CLI+API (`sv verify-auto`, `POST /api/v1/sv/verify-auto`).

> **Audience:** rosf and monono (their gates drive corpora by `top`), anyone who writes `@mununu_guarantee` inside a reused block.
>
> **TL;DR — a verdict-semantics change for one class of rows: an annotation written inside a module that is instantiated under the `--top` now decides instead of skipping.** It is checked once per instance of its module, as a row named `<instance prefix>ann_guarantee_<n>` (label prefixed the same way, atoms qualified with the prefix). Rows in the top module itself are unchanged. Nothing changes for a design verified with the annotation's own module as top.

## What changed

| before | after |
|---|---|
| `sprite_stage.sv` carries `@mununu_guarantee … fill0_q …`; verified under `stage_cdc` (which instantiates it as `u_stage`): `ann_guarantee_N` → `skipped: predicate references unknown register/signal fill0_q` | the row is `u_stage.ann_guarantee_N` (label `u_stage.<name>`), formula over `u_stage.fill0_q`, and it decides |
| a module instantiated twice: one skipped row | two rows, `u_a.ann_guarantee_N` and `u_b.ann_guarantee_N` |
| a module not instantiated under the top | unchanged: the row stays as written and skips on the unbound name; the `annotation-properties` note names the module |

Mechanics: the comment scanner records the `module … endmodule` each annotation sits in; the slang elaboration (the same walk that already emits a sub-instance's `assert property` per instance) records every instance occurrence with its hierarchical prefix; the two meet when the annotation rows merge into the extraction. The atoms are qualified from the **parsed** formula (never a text guess); a name that already carries a `.` is left alone.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Corpora whose `[corpus] top` is a parent of annotated blocks gain rows that used to skip. Row **names** for those rows carry the instance prefix: an `--expect` or a baseline keyed on the old positional `ann_guarantee_N` of a *skipped-under-parent* row must take the prefixed name (rows that already decided keep their names). | Re-record baselines once on the pinned engine; expect new `holds`/`violated` rows where you had `skipped`. |
| **monono** | Same, through rosf. The A2 CDC lockstep corpus (where this was found) gains its two `sprite_stage` rows under `stage_cdc`. | Re-run the lockstep measurement on the pinned engine. |
| **mununu-ui** | No shape change; a row name may contain `.` before `ann_guarantee_`. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes — rows that skipped now decide. | Verdict semantics for sub-module annotations. |
| rosf `docker/Dockerfile*` | Only to pick up the new mununu. | Consumer. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Same. | Same. |

## Provenance

mununu#897 (`found:monono`), from roadmap A2's CDC lockstep measurement. Tests at the seams: the scanner records the module (including the attribute form and a `module` word inside a comment); the slang hierarchy fixture yields `leaf` under five prefixes and `top` under `""`; the expansion gives one row per instance with the side tables (source lines, `assumes=ignore`, template forms) following the new names and names a module absent under the top; the qualifier prefixes atom registers and nothing else. The end-to-end (`child` instantiated twice under `parent`: two `holds` rows named by instance; the same annotation `holds` with `child` as top) is `#[ignore]`d for the slang image.

## Not covered here

A `@mununu_template` instance written inside a submodule (the same expansion applies only to `@mununu_guarantee` rows in this change); a module instantiated under the top *and* verified as its own top in one run (not a thing); an annotation that names a register of a *different* module (not supported before either).
