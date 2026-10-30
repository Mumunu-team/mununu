# Consumer briefing — every row gains `source` and `involves`; the sv2v path's legend gets its assignment lines (2026-10)

> Source of truth: [`property_sources_of`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L1079) + [`attach_involves`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L1862) + [`source_index_from_ast`](../../crates/mununu-core/src/adapter/slang/mod.rs#L520) + [`PropertyVerdictView`](../../crates/mununu-core/src/api/models.rs#L2896) — surface: (CLI+API) — `mununu sv verify-auto` (text and `--json`) and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono, mununu-ui, and anyone reading `sv verify-auto` rows or traces.

## TL;DR

- **Every row says where the property is written.** JSON `source: {file, line}`; text `source: pump_pair_sva.sv:19` under `label:`. An SVA row's line is its assertion statement (slang's AST, joined on the row's positional `name` *and* its `label`, so a mismatch gives no line rather than a wrong one); an `@mununu_guarantee` row's is its comment. A template row has none. Absent when unknown.
- **Every row lists the RTL lines it is about.** JSON `involves: [{file, line}]`; text `involves: pump_pair.sv:24-26, 31-33, 36-37, 40, …` under `formula:` / `translated:`. These are the declaration, the `always` block(s) and every writing statement of each register, input and named wire in the property's cone of influence, sorted. It is the cone's code, not a causal slice of a counterexample. Absent when the source map placed nothing in the cone.
- **The sv2v path's legend is complete.** Before, `--frontend sv2v` dropped every assignment line (`source map: partial — the assignment lines are unavailable …`). Now slang's AST supplies them (the edge-triggered `always` blocks assigning each register), so on a design whose names sv2v keeps the status is `available` and the reason is gone. A name the lift invented (`sv2v_cast_…$result`) stays without a line and is listed: `no assignment line for … (sv2v rewrote the source, …)`.
- **Fixed on the way:** the AST walk followed a `for` generate's iterations under the wrong prefix and skipped an instance whose body slang serialised by address; it now names them `gen$0.…`, `gen$1.…` as the lift does.
- Verdicts, exit codes, every other field: unchanged. One more slang subprocess per run was already there (#832); this reads more from its output.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | two optional keys per row (`source`, `involves`); `diagnostics.source_map.status` may now read `available` on an sv2v lane that read `partial` | none required; a gate that compared the whole row object must ignore the new keys |
| **monono** | same; a lane preprocessing with sv2v gets `trace.sources[].assigned` | none required |
| **mununu-ui** | `PropertyVerdictView` gains `source?` and `involves?` | regenerate types from `public/openapi.json`; show `source` beside the label and `involves` as a collapsible list |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the index, the join and the post-pass live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the image is where the sv2v and slang front ends both run |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `slang::declaration_tests::the_source_index_reads_assignments_blocks_and_generate_iterations_from_real_slang` (real slang 11.0 output, `testdata/source_index_forms.sv`: `always_comb` writes but is not a flop block, `always_ff` and `always @(posedge)` are, a bit-select writes its array, a concatenation each operand, `gen$0` / `gen$1` with a by-address body) | pass |
| `slang::declaration_tests::the_index_names_each_assertion_as_the_translator_names_its_row` (the seam: index names and labels = the translator's rows on the same AST) | pass |
| `btor2::dep_graph::tests::cone_named_signals_reads_states_inputs_wire_aliases_and_outputs`; `verify_auto::source_map_tests::{involves_lists_the_cones_lines_and_nothing_outside_it, a_rows_source_joins_on_name_and_label_and_never_guesses, the_sv2v_path_takes_its_assignment_lines_from_the_ast_and_lists_what_it_cannot_place, declarations_fill_the_gaps_and_settle_the_status}`; `trace::source_tests::source_lines_compact_into_ranges_per_file` | pass |
| the tutorial's pump-pair draft on the host (yosys 0.70, yosys-slang, slang 12.1, sv2v 0.0.13), `--frontend slang` with the bound checker | `u_pump_pair_sva.pump_pair_sva_sva_0`: `source: pump_pair_sva.sv:19`; `alternates`: `source: pump_pair.sv:13`, `involves: pump_pair.sv:24-26, 31-33, 36-37, 40, 42, 44-45, 47-48, 52, 54-57, 59-63`; JSON `source`, `involves`, `source_map: {status: "available"}` |
| the same draft, `--frontend sv2v` (the design alone: this sv2v build rejects the checker's `bind`) | the lift is `read_verilog + sv2v`; the legend is complete (`state_q ← pump_pair.sv:33 (register, assigned at pump_pair.sv:52)`), no `source map:` line; `alternates`' `involves` is line for line the slang run's |
| the pump-pair draft with the checker in `mununu-sva` (slang 11.0, yosys-slang stamping cells only, sv2v) | TO FILL before merge |

## Not covered here

- Combinational statements are listed through the named wires they write; an unnamed intermediate expression has no line of its own, and a `case` / `if` header is not listed (only the assignments under it).
- `involves` is the cone, so on a densely coupled block it can be most of the module; it never claims the lines are where a defect is.
- mununu-ui rendering ships in its own PR.
