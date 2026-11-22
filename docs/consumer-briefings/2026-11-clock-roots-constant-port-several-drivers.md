# Consumer briefing: a constant clock, a port of unknown direction and several drivers are named (2026-11)

> Source of truth: [`clock_domains_with_hierarchy`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and `clock_root_notes` in [`adapter/slang/verify_auto.rs`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — every `sv` verb that lifts; the notes on `sv verify-auto` / `POST /api/v1/sv/verify-auto` and `sv cdc-lint` / `POST /api/v1/sv/cdc-lint`

**Audience:** anyone who lifts SystemVerilog with a tied-off clock, a stubbed or black-boxed clock source, or a pad on a clock net; anyone who reads `notes[]`, `diagnostics.merged_clock_domains` or the lint's `domains`, or gates on note kinds. It follows [the briefing of mununu#1148](2026-11-clock-rooted-at-black-box-note.md) and closes the cases that one left open.

## TL;DR

- **Three clock shapes that lifted silently are refused by default now (mununu#1180), on every `sv` verb, like the free-source clocks of mununu#1148:**
  - a **flop whose clock is tied to a constant** (`wire pclk = 1'b0;`, a sub-instance's clock port tied to `1'b1`). Before, the flop was in no clock domain: it lifted on the model's one clock, updated on every step, and no message or count mentioned it. In the design it never updates on a clock edge (an asynchronous set or reset still changes a flop that has one).
  - a clock net on a **port whose direction the lifted design does not give** — the `inout` of a `(* mununu_missing_module *)` stub, or a cell of a module nothing declares. Before, the last such cell read was named as the driver.
  - a clock net that **more than one cell drives** (two stub outputs; logic and a stub output). Before, the last driver read won: a `$and` and a stub's output on one net was refused as "derived" or lifted as "rooted at black box" depending on the cell order.
- **One classification changes:** a stub that only *reads* an otherwise undriven clock net is no longer named as its driver. The net is `undriven` (note `clock-undriven`), not ``rooted at black box `<cell>` ``. A stub's port directions are now read from the hierarchy snapshot (its declaration), so its declared `output` is the root and its `input` is a reader.
- **The refusal says what the clock is, in its own words, and a remedy that fits:** `the clock of \`q\` is tied to constant 0, which has no edge: the storage cells on it never update on a clock edge in the design … connect the clock in the RTL`; `the clock net \`pclk\` is on a port of black box \`pad\` and the lifted design does not say that any of them drives it … give the port a direction in the black box's declaration (\`output\` if it drives the clock, \`input\` if it reads it), provide the source of the cell that drives the clock, or connect the clock in the RTL`; `the clock net \`pclk\` is driven by more than one cell (\`osc\`, \`pll\`) … leave one driver on the clock net in the RTL`. Each ends with the switch and where it works, as before: `--merge-clock-domains` on `sv verify-auto`.
- **Two new note kinds under that opt-in on `sv verify-auto`, both `soundness-caveat`:** `clock-constant` (`items` = `[the constant as the netlist writes it — "1'b0" or "1'b1", one register it clocks, storage-cell count]`) and `clock-multi-driven` (`items` = `[net, the driver cell types joined by ", ", storage-cell count]`). A port of unknown direction uses the existing `clock-rooted-at-black-box` kind, and **its `items[1]` can now be several cell types joined by `", "`** (before: one cell type or `"cut net"`).
- **The `control-slice` note's detail grows** when the report also has one of the four clock-root notes (a `--cutpoint` on a clock net gives both): after "… (safety + over-approx = sound)." it says that the claim does not extend to the storage cells on that clock and names the clock's note. The kind, level, summary and items are unchanged.
- **`diagnostics.merged_clock_domains` strings and the refusal's domain list have three more endings:** ``, on a port of black box `<cell>`, which may drive it``, ``, tied to constant <value>: never ticks; clocks `<register>` `` (the entry's net is the constant, e.g. `1'b0 (posedge, 1 cell, tied to constant 0: …)`) and ``, driven by more than one cell: `<cell>`, `<cell>` ``.
- **`sv cdc-lint` still lints such a design** (it merges with or without the flag). Its `domains` now come from the lift's own scan, so they have an entry for a constant clock (`net` = `1'b0` / `1'b1`) and agree with the lift on a stub's direction; `notes[]` has one line per such clock, and for a constant one it says that the lint reads those cells' outputs as unclocked and reports no crossing from or to them.
- **No JSON field is added, renamed or removed, and no verdict value changes on a lift that goes through.** A design with one of the three shapes that lifted before is refused now unless you opt in on `sv verify-auto`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A design with a tied-off clock, a pad or an undeclared cell on a clock net, or two drivers on one is refused with the root named; with `merge_clock_domains` on `sv verify-auto` it lifts and carries one note per such clock. A stub that only reads an undriven clock now yields `clock-undriven`, not `clock-rooted-at-black-box`. | Allow-list `clock-constant` and `clock-multi-driven` if you gate on note kinds; if you parse `items[1]` of `clock-rooted-at-black-box` as one cell type, split it on `", "`. A tied-off clock is usually a wiring defect in the harness: fix it there rather than opting in. |
| **monono** | Same. | Same. |
| **mununu-ui** | The notes panel shows the new kinds through the generic path. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the refusals and the notes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the case table of mununu#1148's fix, which listed these shapes as open (mununu#1180). Each was measured on the lift's own netlist in `mununu-sva` on 2026-11-22 before a line of the fix was written; the captured netlists and hierarchy snapshots are the unit fixtures.
- **Tests:** `x1180_a_flop_on_a_constant_clock_is_a_domain_and_is_refused` (the reproducer; fails on the parent, where the flop is in no domain), `x1180_every_open_clock_net_shape_is_classified_from_the_captured_netlists` (one row per captured shape, with and without the hierarchy snapshot), `x1180_the_root_does_not_follow_the_cell_order`, `x1180_each_new_root_is_refused_in_its_own_words_unless_merged_and_reported`, `x1180_each_new_clock_root_has_its_own_note`, `x1180_the_control_slice_claim_stops_at_a_clock_with_no_driver`, and the image test `e2e_every_open_clock_net_shape_is_classified_by_the_real_lift`.
- **Class:** `clock-net-driver` in [`docs/defect-classes.md`](../defect-classes.md).

## Not covered here

- The verbs other than `sv verify-auto` and `sv cdc-lint` share the refusal and take `--merge-clock-domains`, but drop the domains: under the flag a free-source clock is refused there, and a derived or multi-clock design lifts with nothing in the report (mununu#1181, open).
- A known driver or a top-level input that is also on a port of unknown direction: the known driver or the input is the root and the port is read as a reader. That is a decision, not a measurement of what the missing module does: a stub whose `inout` does drive a top-level clock input lifts by default as an input clock, and no report names the stub. Give the port a direction in the stub, or provide the module's source, to settle it.
- Which of several drivers the design resolves to. The scan names them and stops.
