# Consumer briefing: a clock rooted at a black-box output is named (2026-11)

> Source of truth: [`clock_domains`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and the `clock-rooted-at-black-box` note in [`adapter/slang/verify_auto.rs`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — `sv verify-auto`, `POST /api/v1/sv/verify-auto`

**Audience:** anyone who reads `notes[]` or `diagnostics.merged_clock_domains` from `sv verify-auto`, or gates on note kinds.

## TL;DR

- **One new note kind, additive (mununu#1148):** `clock-rooted-at-black-box`, level `scope-caveat`, one per clock net driven by a black-box instance's output (a PLL, a clock generator, a black-boxed or auto-stubbed clock gate). `items` = `[net, cell type, storage-cell count]`. The model ticks such a clock on every step: right for an independent source, neither an over- nor an under-approximation of a cell that gates or divides its input; the scan cannot tell them apart and says so.
- **A diagnostics string grows:** when `diagnostics.merged_clock_domains` is emitted (more than one domain, or a derived one), a black-box-rooted domain's line ends with ``, rooted at black box `<cell>` ``. The CLI's clock-domain refusal message prints the same suffix.
- **No verdict changes.** No JSON field is added or renamed; the API schema is unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Designs with a PLL or a black-boxed clock gate carry one more note per rooted clock. | Nothing, unless you allow-list note kinds: add `clock-rooted-at-black-box`. |
| **monono** | Same. | Same. |
| **mununu-ui** | The notes panel shows the new kind through the generic path. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the note. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the R6 review (mununu#1148).
- **Tests:** `x1148_a_clock_rooted_at_a_black_box_output_is_named_in_the_description` (the reproducer: a flattened-netlist fixture with a black-box clock gate driving two flop clocks, one through an inverter; the description names the root for both and not for the root clock; fails on the parent).

## Not covered here

- The sequential verbs other than `sv verify-auto` (`sv verify-liveness*`, `sv verify-recoverability`) share the scan and the diagnostics string but do not add the note; a follow-up if a consumer needs it there.
