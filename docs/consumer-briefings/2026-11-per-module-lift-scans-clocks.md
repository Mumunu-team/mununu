# Consumer briefing: the per-module SV lift scans clocks and refuses what the flattened lift refuses (2026-11)

> Source of truth: [`refuse_module_clocks`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L3024) — surface: CLI+API — `sv emit-btor2-per-module`, and `mununu verify` / `POST /api/v1/verify` with an `sv-yosys` source set to `multi_module = true`.

**Audience:** rosf, monono, mununu-ui, and anyone who lifts a SystemVerilog design one module at a time or composes it from its instances.

## TL;DR

- **Before, these two paths never looked at a clock.** A module with two clock domains, a gated or divided clock, or a clock no input drives was lifted onto the model's one implicit clock, and nothing said so. The flattened lift of the same source (`sv verify-auto`, `verify` with `multi_module = false`) refused it. A verdict from the per-module path on such a design was a verdict on a design whose registers all step together.
- **Now both paths scan the clocks and refuse what the flattened lift refuses, in its words.**
  - `sv emit-btor2-per-module` scans each module as the design of its own that its BTOR2 file is. The refusal names the module first.
  - `multi_module = true` scans the composed design first and refuses it word for word as `multi_module = false` does. It then scans each module as lifted.
- **Neither path has an opt-in to merge clock domains, and the refusal says so.** The fix is on the design (a clock enable in place of a gated clock, one domain at a time), `mununu sv cdc-lint` for the crossings, or the flattened lift, which takes `--merge-clock-domains` on the verbs the message lists.
- **`--output-dir` is written later.** The BTOR2 files reach the directory once every module has lifted and passed its clock scan. A clock refusal or a failed yosys run leaves no file there. The per-module yosys log is no longer written there.
- **No route, flag or JSON shape changes.** Runs that lifted a single-clock design give the same models and verdicts.

## What you will see

Measured 2026-11-23 in `mununu-sva` (yosys 0.68+120), with a CLI built from this change.

`sv emit-btor2-per-module` on a module with two clock inputs, exit code 1, nothing in `--output-dir`:

```text
sv emit-btor2-per-module: adapter/yosys: module `child`, lifted as a design of its own: the design has 2 clock domains (clk_a (posedge, 1 cell), clk_b (posedge, 1 cell)); the model has one implicit clock, so lifting it would step every domain together and hide every crossing defect. Run `mununu sv cdc-lint` to classify the crossings, or verify each domain on its own; this operation has no opt-in to lift the design as it is (a mununu limitation); elsewhere, `--merge-clock-domains` on `sv verify-auto` / `sv verify-liveness` / `sv verify-liveness-all` / `sv verify-recoverability` / `sv check-fsm` / `sv lint` / `sv mutate` (API `lift.merge_clock_domains`) accepts that under-approximation explicitly, and the report then says so in a `clock-domains-merged` note.
```

`mununu verify` with `multi_module = true` on a design with a gated clock, exit code 1. The text after `adapter/yosys:` is what `multi_module = false` prints for the same design:

```text
source `system` adapter `sv-yosys` failed to translate: adapter/yosys: the clock net `u_c.gclk` is derived from logic (driven by `$and`: a gated, divided or muxed clock); the model has one implicit clock that ticks on every step, so lifting it would ignore the logic that gates or divides the clock and every verdict could be wrong. …
```

One case is new to the composed path. The per-module lift lifts each module type on its own, with its default parameters (mununu#1248). A design can instantiate a module with its clock gate switched off by a parameter, so the design has one clean clock and the module as lifted has a gated one. The design scan passes and the module is refused, with what to do:

```text
… adapter/yosys: module `stage`, lifted on its own with its default parameters: the clock net `gclk` is derived from logic … The per-module lift lifts each module on its own with its default parameters, and the clocks of the design as a whole were accepted, so this is not the module as the design instantiates it (a mununu limitation). `multi_module = false` on the source lifts the flattened design instead.
```

On that design `multi_module = false` lifts and answers (measured, the same build).

## What each scan refuses

| | `sv emit-btor2-per-module` (each module on its own) | `multi_module = true` (the modules composed) |
|---|---|---|
| the design has two clock domains | not asked: the verb returns modules, not the design | refused, the flattened lift's words |
| a module has two clock inputs that the design ties to one clock | refused: on its own the module has two domains | accepted: the design has one |
| a module has two clock inputs that the design drives from two clocks | refused | refused by the design scan |
| a module's clock is gated, divided, constant, undriven or cut, as the module is lifted | refused, the module named | refused, the module named, with `multi_module = false` as the fix |

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A composition (`multi_module = true`) or a per-module emission of a design with more than one clock domain, or with a derived clock, is now refused with the reason. Before it returned a model and verdicts. | Treat earlier per-module verdicts on such designs as unsupported. Re-run them with `multi_module = false`, or fix the clocking the message names. A refused run writes nothing to `--output-dir`; files an earlier run left there stay as they were. |
| **monono** | You use the whole-design lift (`sv verify-auto`); nothing changes. | None. |
| **mununu-ui** | The per-module path is not wired in the UI. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle | Yes, if you use the per-module or multi-module paths. | The fix is in the binary. |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, for the same paths in the image. | The binary on the volume changes; the toolchain does not. |
| `mununu/docker/Dockerfile.dev` | Only if your dev workflow lifts per module. | The fix is in the binary. |
| `mununu/docker/Dockerfile.profile`, `Dockerfile.extract*` | No. | They don't lift SV per module. |
| rosf `docker/Dockerfile*` | Yes, if rosf lifts or composes SV designs per module. | Picks up the refusals. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Not callers of this path. |

## Provenance

- **mununu#1191**, class `clock-net-driver` in [`docs/defect-classes.md`](../defect-classes.md). Found internally, reading which lift entries return a model without the clock scan.
- **Tests.** In the image: `e2e_x1191_the_per_module_lift_refuses_what_the_flattened_lift_refuses` runs both per-module entries beside the flattened lift on every refused shape, on the tied-clock module, on the default-parameter case, and on what reaches `--output-dir`. Without yosys: the `x1191_…` lib tests pin the script text, the wording and the snapshot reader, and the policy test `per_module_lift_scans_clocks_1191` lists every script that writes a model, every yosys run and every function that returns a lifted model with the clock scan it makes.
- engine: none. The change is in the lift (yosys scripts and the clock scan); no verification engine is involved.

## Not covered here

- **The per-module lift still uses each module's default parameters** (mununu#1248) **and still leaves the top's own logic out of the composed model** (mununu#1249). Both can give a wrong verdict under `multi_module = true` on a single-clock design, and this change fixes neither. Until they are fixed, `multi_module = false` is the lift to trust for a design with a parameter override on an instance or with logic in its top.
- A module with no body is not cut by the per-module lift; yosys's own error ends the run (mununu#1215).
- A module that mununu's own BTOR2 reader cannot take (more input combinations per step than it enumerates) still fails `sv emit-btor2-per-module` after the files are written, as before.
