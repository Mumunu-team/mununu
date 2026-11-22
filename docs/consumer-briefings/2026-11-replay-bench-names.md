# Consumer briefing: the replay testbench holds every name it writes to a grammar (2026-11)

> Source of truth: [`bench_refusal`](../../crates/mununu-core/src/adapter/verilator/replay.rs#L175) — surface: CLI+API — `sv verify-auto --emit-testbench DIR [--tb-clock PORT] [--replay]` / `POST /api/v1/sv/verify-auto` with `emit_testbench: true` (and `tb_clock`, `replay`)

**Audience:** anyone who replays a counterexample under Verilator (`--emit-testbench`, `--replay`, `emit_testbench` / `replay` over HTTP), who passes `--tb-clock` / `tb_clock`, or who reads `trace.replay.testbench.skipped`.

## TL;DR

- **What it is.** The generated `tb.cpp` and `replay.gtkw` write the design's names as they are: the clock, the pinned reset, the input ports the trace drives and the registers it compares. The top module's name and the source paths were already held to what the `Makefile` can carry (mununu#1176); the other names were written as given. Each is now held to the grammar of the place it is written.
- **No verdict changes.** Only the Verilator cross-check and what is said about it.
- **The clock** (`--tb-clock PORT`, `tb_clock`) is a port name: a letter or `_`, then letters, digits and `_`. With another value no testbench is generated for the run, and every row with a counterexample says why, as for a source path the `Makefile` cannot name: the text report prints `replay testbench: not written — <why>`, the JSON row has `trace.replay.not_generated`, and with `--replay` / `replay: true` `trace.replay.result.outcome` is `unavailable` with the same reason. The reason names the flag (CLI) or the field (API). The default search (`clk`, `clk_i`, `clock`, `i_clk`, `aclk`, `clk_sys`) is unchanged.
- **The pinned reset** (`diagnostics.gated_resets`, found by the lift) is held to the same grammar; one outside it means no testbench, said as a mununu limitation.
- **An input port** the trace drives whose name is outside that grammar is left undriven, and **a register** whose flat name is not port names joined by `.` with element indices at the end (`u_sub.cnt`, `mem[3]`) is not compared. Both are listed in `skipped` (and in the `// not compared:` header of `tb.cpp`) with the reason; the rest of the testbench is written. A register with an escaped name used to be written into `tb.cpp` as it came, and the testbench did not compile.
- **`replay.gtkw`** lists only the signals the testbench could name.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A `tb_clock` that is not a port name yields rows with `trace.replay.not_generated` and no `testbench`; `testbench.skipped` can carry two new reasons. | Pass the clock port's plain name in `tb_clock`, or omit it for the default search. No change for a request that already does. |
| **monono** | The same through `--tb-clock`; the text report's `replay testbench: not written — …` line can now name the clock. | None for a lane that passes a port name. |
| **mununu-ui** | `sv verify-auto` is not a CTXDSL capability. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** an internal read of the replay generator while extending the mununu#1176 case table.
- **Class:** `lift-script-path` in [`docs/defect-classes.md`](../defect-classes.md), which holds the case table.
- **Tests:** `a_clock_name_that_is_not_a_port_name_yields_no_bench` (the reproducer) and the enumerating `every_name_the_bench_writes_is_held_to_its_grammar` (the clock, the reset, an input port, a register and the save file against one list of names outside the grammar), both in `adapter/verilator/replay.rs`.
- **API schemas:** `docs/api-schemas/` updated for the descriptions of `tb_clock` and `trace.replay.not_generated`; no shape change.
- **Measured** (2026-11-22, the `mununu-sva` image, Verilator 5.051, this change against its parent `b821c044`, debug builds), `sv verify-auto <design> --top <top> --emit-testbench DIR --replay` on a two-register design whose assertion is violated at depth 5:
  - with the default clock: `replay: match` on both builds;
  - with `--tb-clock "clk x"`: the parent writes a testbench that does not compile and the row says `replay: unavailable` with the compiler's output; this change writes no testbench, prints ``replay testbench: not written — the clock `clk x` is not a port name the bench can drive (a letter or `_`, then letters, digits and `_`) — pass `--tb-clock` the name of the top module's clock port``, and the JSON row has that text in `trace.replay.not_generated` with `result.outcome: "unavailable"`;
  - with one of the two registers declared under an escaped name (`\q-prev `): the parent writes a testbench of 12 checks that does not compile (`replay: unavailable`); this change writes one of 6 checks, lists `q-prev` in `skipped` with its reason, and the replay says `match — 0 of 6 checks disagree`;
  - with the two registers inside a generate loop: unchanged. The lift names them `g$0.r` and `g$1.r`, and both builds list them as `not an RTL signal` and compare nothing.

## Not covered here

- A register inside a generate loop is still not compared (see the last measured row): it is skipped, not read through Verilator's own name for it.
- A clock port with an escaped name cannot be replayed; there is no way to name it to the testbench.
