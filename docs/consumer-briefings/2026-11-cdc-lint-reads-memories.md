# Consumer briefing: `sv cdc-lint` reads memories — a write port captures, a stored word is a source (mununu#1230, 2026-11)

> Source of truth: [`cdc_lint`](../../crates/mununu-core/src/adapter/cdc_lint.rs#L202) — surface: CLI+API — `mununu sv cdc-lint` / `POST /api/v1/sv/cdc-lint`

**Audience:** anyone who runs `sv cdc-lint` on a design with a memory (an array written in a clocked block) and more than one clock, and above all anyone holding a `holds` from it on such a design: an asynchronous FIFO, a register file written from another domain, a RAM whose address or enable comes from another clock.

## TL;DR

- **A soundness bugfix on the lint.** The lint walks the flattened design from storage to storage through logic. It read registers and logic cells. It did not read a memory: a memory cell ended every walk that reached it, and the report said `holds` about what lay behind it.
- **What that gave**, each measured on a small hand-written design (`mununu` built from `main` at `df663d93`, the yosys of `mununu-sva`, 0.68+120, 2026-11-23):
  - a value from another clock captured by a write port's enable, data or address: no row, `holds`;
  - a word written on one clock and registered on another (the read side of an asynchronous FIFO): no row, `holds`. The documented `dual-clock-memory` row compared a memory's read clock with its write clocks, and the lift gives a memory no read clock, so the row never appeared;
  - a read address from another clock: no row, `holds`;
  - a design whose second clock only writes a memory: "one clock net", `holds`;
  - a two-flop synchroniser whose first stage is also written into a memory: the row `synchronized`, `holds`. With a register in the memory's place the same design is `unsynchronized`, `violated`;
  - a multi-bit crossing whose source register is loaded from a memory of another clock: the gray-step check ran and decided the row. The check is exact only when the source's cone lies in its own domain, and the twin design with a register in the memory's place stays `unknown` for that reason. A wrong `holds` from this was not produced: the designs tried came out `violated`.
- **Now** a memory is read as what it is made of. A clocked write port is a storage cell on its write clock, and its enable, address and data are what it captures. A read port is logic over its address and the stored word. So the rows above are reported with the kinds the lint already had: `unsynchronized`, `combinational-before-sync`, and `dual-clock-memory` for a register that reads, on its own clock, a word stored on another.
- **`dual-clock-memory` means something you can find in the design.** The row's `source` is the memory and its `destination` the register that reads the word (or the memory itself, when it is written on more than one clock). It stays `unknown`: whether a word is read while it is written depends on the pointers, which is a functional property and not one a structural lint decides.
- **A row can also go from `violated` to `unknown`:** the multi-bit row above is no longer decided.
- **No JSON field, kind, flag or route is added, renamed or removed.** A row's `source` or `destination` can now be a memory's name (`m`, `u_ram.m`). Two notes are new in wording: a memory cell the lint cannot read is named in a note that asks for a report, and a memory port written without a clock is under the existing latch note.
- **The exit code follows the worst row**, so a CI step that passed on such a design fails now (`--fail-on`, exit 2 on a `violated` row by default).

## What to check

A `holds` you hold from `sv cdc-lint` is affected when the design has a memory and more than one clock, and the memory is written on one clock while anything it reads or feeds belongs to another. Run the lint again with this build. A design with no memory, or with one clock, gets the report it got before.

The defect was found by reading the lint while fixing mununu#1229 and measured on hand-written designs. It was not met on a consumer's design.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `sv cdc-lint` on a design with a memory between two clocks goes from `holds` to `violated` or `unknown`, with rows that name the memory. A `synchronized` row can become `unsynchronized`, and a decided multi-bit row can become `unknown`. The exit code follows. | Run the lint again on such designs. Read a `dual-clock-memory` row as "prove the pointer protocol": the functional recipe in `docs/verifying-rtl.md` is the way. |
| **monono** | Same. | Same. |
| **mununu-ui** | Nothing: an RTL verb, not a CTXDSL capability. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. No tool was added to or removed from any image. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

No cache directory needs clearing: the lint reads the snapshot the lift already stores, and that snapshot does not change.

## Provenance

- **Found by:** the work on mununu#1229, whose memory case listed the second domain and still reported no crossing. The fourteen designs and the control are in the issue (mununu#1230).
- **Class:** `netlist-cell-kind-coverage` in [`docs/defect-classes.md`](../defect-classes.md), which holds the table of cell kinds and what the lint does with each.
- **Tests:** `x1230_every_way_a_memory_takes_part_in_a_crossing_has_its_row` (fifteen designs on the snapshots the lift's script gives, each with its rows; the reproducer) and `e2e_x1230_every_memory_case_has_its_row_through_the_real_lift` (the same sources through the real lift); `x1230_a_word_stored_on_another_clock_is_in_a_register_s_cone` (the gray-step precondition); `x1230_a_clocked_read_port_is_a_register_on_its_read_clock`, `x1230_a_memory_with_no_write_port_is_logic_over_its_read_address`, `x1230_an_unclocked_write_port_is_under_the_latch_note`, `x1230_a_memory_cell_the_lint_cannot_read_is_named_in_a_note`; the cell kinds the lint does not follow: `x1230_storage_the_lint_does_not_follow_is_under_a_note_or_stores_nothing`, `e2e_x1230_a_value_through_a_black_box_is_under_the_black_box_note`.
- **API schemas:** no shape change.

## Not covered here

- **The lint does not decide a `dual-clock-memory` row.** It is `unknown` on a correct asynchronous FIFO and on a broken one alike.
- **A memory is one stored word to the lint.** It does not tell one address from another: a word written at one address and read at another is the same row.
- **A memory with a registered read port inside the cell** (what yosys's `memory_dff` makes) is read as a register on the read clock. The lift's script does not make one today; the case is tested on a hand-made snapshot only.
- **A memory written in a latch-style block** is latches after yosys, under the latch note as before.
- **Rows name a register by the first of its names in the snapshot**, which can be an output port the register drives and not the name in the source (the clock twin of this is mununu#1231).
- **Cost.** The lint's time on a large memory design was not measured.
