# Consumer briefing — every counterexample names where its cells live in the RTL (2026-10)

> Source of truth: [`cell_sources`](../../crates/mununu-core/src/adapter/yosys/mod.rs) + [`TraceNames::tidy`](../../crates/mununu-core/src/trace.rs) + [`SourceMapView`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API) — `mununu sv verify-auto` (text and `--json`) and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono, mununu-ui, and anyone who reads a counterexample back to the RTL.

## TL;DR

- **A trace now ends with a legend.** Text: a `cells:` block after the steps, one line per cell — `state_q ← pump_pair.sv:32 (register, assigned at pump_pair.sv:52)`, `flow ← pump_pair.sv:25 (input)` — the first step's registers then its inputs. JSON: `trace.sources` = `[{cell, kind, declared: {file, line}, assigned: [{file, line}]}]`, omitted when empty.
- **The map's status is on the report.** `diagnostics.source_map` = `{status: "available"}` or `{status: "unavailable", reason}`; the text report prints `source map: unavailable — <reason>` under `model:` and no legend. Both yosys-slang (`--frontend slang`, the image's default) and yosys's own `read_verilog` (`--frontend verilog`) stamp the RTL's file and line; the map is unavailable only when sv2v preprocessed the source (`--frontend sv2v` / `use_sv2v`), whose output's lines are not the RTL's.
- **Nothing else moves.** Verdicts, exit codes, the trace's steps, names and values, the replay object: unchanged. `ModelDiagnosticsView` gains one required key, `source_map`; `Trace` gains one optional key, `sources`. Every schema embedding `Trace` is regenerated.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | traces gain `sources`; `diagnostics.source_map` appears on every verify-auto report | none required; a report reader can link a cell to `file:line` |
| **monono** | same; a lane that preprocesses with sv2v sees `unavailable` with the reason, never an empty legend | none required |
| **mununu-ui** | regenerate the API types (`public/openapi.json` → `generate:types`); render the legend under the trace table | a follow-up PR |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the lift-side join lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the verify-auto path consumers run; yosys-slang is what stamps the lines |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `yosys::cell_source_tests::cell_sources_reads_declarations_flops_and_ports_and_skips_wires` (a netlist trimmed from the pump-pair lift: ports, declarations, two `$aldff` flops on line 52 with one merged `src`, a `$mux`, a hidden net) and `the_staged_primary_is_named_after_the_users_file` (the lift stages the primary as `work.sv`; the legend names the user's file) | pass |
| `trace::source_tests::the_legend_lists_the_traces_cells_with_their_lines_in_trace_order` and `a_yosys_src_attribute_parses_to_its_file_and_first_line` | pass |
| the drift suite after regeneration (every schema embedding `Trace`, plus the diagnostics view); `cargo check --workspace --all-targets`; clippy `-D warnings` on core + cli | pass / clean |
| `sv verify-auto --frontend slang` on the tutorial's pump-pair draft (private material; host build, the YosysHQ bundle's yosys 0.70 + yosys-slang) | the violated `alternates` row ends with `cells:` — `lof_q ← pump_pair.sv:36 (register, assigned at pump_pair.sv:52)`, `start_a ← :31`, `start_b ← :32`, `state_q ← pump_pair.sv:33 (register, assigned at pump_pair.sv:52)`, then `flow ← :26 (input)`, `start_cmd ← :24`, `stop_cmd ← :25`; line 33 is `output logic [1:0] state_q`, 36 is `logic [2:0] lof_q;`, 52 is the `always_ff @(posedge clk or negedge rst_n)`; JSON `trace.sources` carries the same seven entries and `diagnostics.source_map` is `{status: "available"}`; verdicts and exit code 2 unchanged |
| the same design through the other front ends | `--frontend verilog` (yosys's own `read_verilog`, no sv2v; the draft alone, since that parser rejects the bound SVA file): the same legend, line for line — `read_verilog` stamps the RTL's lines too; `--frontend sv2v`: `source map: unavailable — sv2v preprocessed the source before yosys read it, so the netlist's lines are sv2v's, not the RTL's; lift without sv2v (--frontend slang)` under `model:`, no legend, `diagnostics.source_map.status = "unavailable"` with that reason |

## Provenance

- Request (user, 2026-10-29): RTL line numbers for the code involved in a counterexample; a map from RTL to BTOR2 states to counterexample states. The join: yosys-slang stamps every net with its declaration and every flop with its `always_ff` in the flat netlist JSON the lift already keeps for the trace aliases; BTOR2 states are the net names; the trace's cells are those names after aliasing.
- Design and the per-format consistency matrix: `.claude/plans/rtl-source-map-2026-10-29.md` (private); the per-property `involves` list and the property's own line (slang `--ast-json-source-info`) are the next PR.

- Two defects the measured runs caught before the PR: the legend first read `work.sv:36`, the lift's staging name for the primary source, now renamed to the user's file; and the map was first refused on the whole Verilog front end, although yosys's own `read_verilog` stamps lines as yosys-slang does — the gate is now "sv2v ran", recorded by the lift.

## Not covered here

- The lines name where a cell is declared and assigned, not which branch of the `always` block produced the value at a given cycle; a causal slice of the path is future work.
- Registers whose net name yosys mangled (`…$result`, `gen$k.…`) resolve only when the mangled name is itself a netname with a `src`; the legend lists what resolves and omits the rest.
- The property's own source line (the assertion or annotation) is not in this change.
