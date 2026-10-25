# Consumer briefing — the design's names in `sv verify-auto` traces and `sv mutate --list` (2026-10)

> Source of truth: [`TraceNames`](../../crates/mununu-core/src/trace.rs), [`trace_aliases`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and [`list_targets`](../../crates/mununu-core/src/adapter/btor2/mutate.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every client that reads `properties[].trace` from `sv verify-auto`. Additive to the 0.5 `Trace` ([`2026-10-trace-type-0.5.md`](2026-10-trace-type-0.5.md)); no verdict changes.

## TL;DR

- **`steps[].state` lists the design's registers only.** The cells the verifier adds to the model now sit in a new `steps[].monitor` list: the property monitor's history latches (`mununu_wit_…`), `$past` shadows (`x__past`), fault-rewrite state (`mununu_fault_…`) and the bit / popcount expansions of relational atoms. On a `(running && !flow)[*6] |=> lof_trip` row, 27 of the 31 cells per step were monitor latches.
- **A register named through a bound checker shows the design's name.** `bind m m_sva u_sva (.*)` gives a design register a second name, and the BTOR2 state could come out as `u_sva.state_q`. The trace now prints `state_q` when the flattened netlist has a top-level net with the same bits and the same leaf name. Only the displayed name changes, never a value.
- **The clock is no longer listed among `steps[].inputs`.** The model steps the clock implicitly, so `clk=0` on every step carried no information.
- **`sv mutate --list` (CLI and `/api/v1/sv/mutate` with `list`) names targets the same way.** A register listed as `u_x_sva.reg` is now listed as `reg` when that name reaches the same cell, and an `invert_cond` entry that is a bound checker's port mirroring a listed signal or a primary input (`u_x_sva.clk`, `u_x_sva.rst_n`, `u_x_sva.tripped_q` beside `tripped_q`) is no longer listed. Applying a mutation is unchanged: either spelling resolved before and still does.
- **The CLI text** prints the same tidy state and adds one line per counterexample counting the hidden monitor cells, which `--json` lists under `steps[].monitor`.
- **Unchanged:** verdicts, `decided_by`, the Verilator replay testbench (it keeps the raw names it needs), and the traces of the other verbs.
- **Schemas:** every response that embeds a `Trace` gains the optional `TraceStep.monitor` (`sv-verify-auto-response`, `btor2-verify-response`, `btor2-game-response`, `cegar-response`, `context-eval-response`, `context-synth-response`); it is only ever filled on `sv verify-auto` rows.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | reads `properties[]` verdicts, not trace cells | none; recapture fixtures at your next pin if they include a trace |
| **monono** | lanes read verdicts by name, not trace cells | none |
| **mununu-ui** | a trace renderer that lists `state` now shows fewer, design-named cells | optionally render `monitor` behind a toggle |
| **anyone matching a cell or a mutation target by its bind-port name** (`u_x_sva.reg`) | named `reg` when the design has that net | match the design's name; `--mutation stick:u_x_sva.reg` still applies |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | the trace shape |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts read verdicts and timings |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | at the next pin | no flag or verdict change |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | at the next pin | no flag or verdict change |

## Verification steps for every consumer

```bash
mununu sv verify-auto pump_seq.sv --source pump_seq_sva.sv --top pump_seq --pin rst_n=1 --json \
  | jq '.properties[] | select(.trace) | .trace.steps[0] | {state: [.state[].register], monitor: (.monitor | length), inputs: [.inputs[].register]}'
#   state names the design's registers (state_q, not u_pump_seq_sva.state_q), monitor counts the
#   added cells, inputs has no clk
```

## Provenance

- Found preparing the EmTech tutorial recordings: the `pump_seq` loss-of-flow counterexample was unreadable on both surfaces. Tests at the seams: `trace::tests` (the monitor-cell classification and the alias / clock rules), `adapter::yosys::tests::trace_aliases_map_a_bound_port_to_the_designs_net` (on the flattened-netlist shape measured with `MUNUNU_KEEP_YOSYS_TMP=1`), `api::models::bottom_reason_view_tests::the_trace_shows_the_designs_registers_and_keeps_the_monitor_apart` (the conversion both surfaces use), and `adapter::btor2::mutate::tests::list_targets_names_a_bound_register_by_the_designs_name_and_skips_mirrored_ports`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The traces of `btor2 verify`, cegar and the game, which have no bound checker and no witness monitor; their `monitor` stays empty.
