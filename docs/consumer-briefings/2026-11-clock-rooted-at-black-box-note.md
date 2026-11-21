# Consumer briefing: a clock rooted at a black-box output is named (2026-11)

> Source of truth: [`clock_domains`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and the `clock-rooted-at-black-box` note in [`adapter/slang/verify_auto.rs`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — `sv verify-auto`, `POST /api/v1/sv/verify-auto`

**Audience:** anyone who reads `notes[]` or `diagnostics.merged_clock_domains` from `sv verify-auto`, or gates on note kinds.

## TL;DR

- **Two new note kinds, additive (mununu#1148), both `soundness-caveat`:** `clock-rooted-at-black-box`, one per clock net whose root is the output of a black box the netlist still holds (a deleted `(* mununu_missing_module *)` stub; `items` = `[net, cell type, storage-cell count]`) or a cut net (`$anyseq` from `cutpoint -blackbox` on a declared or empty black box — a PLL, a clock gate under `--empty-blackboxes` — or a `--cutpoint`; `items[1]` = `$anyseq`); and `clock-undriven`, one per clock net with no driver (`items[1]` empty). The model ticks such a clock on every step: right for an independent source, neither an over- nor an under-approximation of a cell that gates or divides its input or a net nothing drives; the scan cannot tell them apart and says so.
- **One lift that was refused now lifts with the note.** A design whose flop clock came from a cut black box was refused as "derived from logic (driven by `$anyseq`)"; a free source is not logic, so the single-domain case lifts and carries the note. A design with a `$`-cell-derived clock is still refused; two domains are still two domains.
- **A diagnostics string grows:** when `diagnostics.merged_clock_domains` is emitted (more than one domain, or a derived one), a free-source domain's line ends with ``, rooted at black box `<cell>` ``, ``, rooted at a cut net (a black-box output or a cut point)`` or ``, undriven``. The CLI's clock-domain refusal message prints the same suffix.
- **No verdict value changes; one more design lifts.** No JSON field is added or renamed; the API schema's description of `merged_clock_domains` is updated.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Designs with a PLL, a cut or stubbed clock gate, or an undriven clock carry one more note per such clock; a cut-black-box clock no longer refuses the lift. | Nothing, unless you allow-list note kinds: add `clock-rooted-at-black-box` and `clock-undriven`. |
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
- **Tests:** `x1148_a_clock_rooted_at_a_black_box_output_is_named_in_the_description` (the reproducer, on the netlist captured from the lift's own script in `mununu-sva`: a deleted missing-module stub's output; fails on the parent), `the_two_black_box_shapes_are_free_roots_not_derived_domains` (the stub and the cut shapes, both captured), `every_clock_root_kind_is_classified_and_described` (exhaustive over the root kinds), `x1148_a_free_source_clock_is_a_soundness_caveat_with_its_cell` and `x1148_the_clock_root_note_survives_the_notes_rebuild` (the note at its seam).

## Not covered here

- The verbs other than `sv verify-auto` (`sv verify-liveness*`, `sv verify-recoverability`, `sv cdc-lint`, `--plan-only`) share the scan and the diagnostics string but do not add the note; a follow-up if a consumer needs it there.
- An `inout` port of a black box and a bit with two drivers are not classified (open rows of the `clock-net-driver` class).
