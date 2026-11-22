# Consumer briefing: a clock rooted at a black-box output is named (2026-11)

> Source of truth: [`clock_domains`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and the `clock-rooted-at-black-box` note in [`adapter/slang/verify_auto.rs`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — `sv verify-auto`, `POST /api/v1/sv/verify-auto`

**Audience:** anyone who reads `notes[]` or `diagnostics.merged_clock_domains` from `sv verify-auto`, or gates on note kinds.

## TL;DR

- **A clock the lifted design holds no driver for is refused by default on every `sv` verb (mununu#1148), like a derived clock:** the output of a black box the netlist still holds (a `(* mununu_missing_module *)` stub you supplied for a clock gate), a cut net (the output of a declared or empty black-box module — a PLL, a clock-gate model with no body — or a `--cutpoint` on the clock net), or a net nothing drives. The refusal names the net, the root and the remedy, and ends with the switch and where it works: `--merge-clock-domains` on `sv verify-auto`, whose report names the root (`sv cdc-lint` lints such a design with or without the flag and names the root too — next item); on the six other `sv` verbs and the API routes that take the flag it does not lift such a clock (refused, the message names the two verbs that do). Before: the declared / empty case was refused as "derived from logic (driven by `$anyseq`)" with no remedy that fit; the stub case and the undriven-net case **lifted silently** with the flops ticking on every step — the defect of #1148.
- **Two new note kinds under that opt-in, both `soundness-caveat`:** `clock-rooted-at-black-box` (`items` = `[net, cell type or "cut net", storage-cell count]`) and `clock-undriven` (`items[1]` empty), one per such clock net, beside the existing `clock-domains-merged` caveat; `diagnostics.merged_clock_domains` lists the domains with their roots, and the `clock-domains-merged` caveat's summary says "a clock with no driver in the lifted design" for a lone such domain. The other `sv` verbs take the flag but drop the domains: for a free-source clock the opt-in is refused there (the message names `sv verify-auto`); for a derived or multi-clock design it still lifts with nothing in the report — mununu#1181, pre-existing.
- **`--cutpoint` on a clock net** is refused by default too, as a cut root (before: refused as "derived from logic (driven by `$anyseq`)"; it was accepted only at this PR's second head). Under the opt-in on `sv verify-auto` it lifts with the note beside the `control-slice` caveat, whose "a definite HOLDS transfers" does not hold for a clock — an open row of mununu#1180.
- **`sv cdc-lint` (and `POST /api/v1/sv/cdc-lint`) still lints such a design** — it forces the merge and reads the domains — and now adds one `notes[]` line per such clock: "clock `<net>` is driven by … : crossings from or to it are classified as from an independent clock, which holds for a PLL and not for a gated or divided copy of another domain's clock". Before, it linted with no word about the root. On the other `sv` verbs and `mununu verify` with an SV source the default refusal applies; under `--merge-clock-domains` they lifted such a clock before and now refuse it.
- **A diagnostics string grows:** when `diagnostics.merged_clock_domains` is emitted (more than one domain, a derived one, or a free-source one), a free-source domain's line ends with ``, rooted at black box `<cell>` ``, ``, rooted at a cut net (a black-box output or a cut point)`` or ``, undriven``. The refusal message prints the same suffix.
- **No verdict value changes on a lift that goes through.** A design that lifted silently with a stubbed clock gate is refused now unless you opt in. No JSON field is added or renamed; the API schema's description of `merged_clock_domains` is updated.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A design whose clock comes from a stubbed clock gate, a black-box PLL or a cut net is refused with the root named; with `merge_clock_domains` it lifts and carries one more note per such clock. | Pass `merge_clock_domains` where the lockstep reading is intended; allow-list `clock-rooted-at-black-box` and `clock-undriven` if you gate on note kinds. |
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

- The verbs other than `sv verify-auto` share the refusal and the diagnostics string; they take `--merge-clock-domains` too, and under it a derived or multi-clock design lifts there with nothing in the report (mununu#1181); a free-source clock does not (refused).
- A constant clock bit, a net between two stubs, an `inout` port of a black box and a bit with two drivers: mununu#1180 (open rows of the `clock-net-driver` class).
