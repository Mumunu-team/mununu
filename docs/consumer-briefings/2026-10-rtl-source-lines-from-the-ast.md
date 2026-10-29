# Consumer briefing — the legend's declaration lines come from slang's AST on every front end; `source_map` gains `partial` (2026-10)

> Source of truth: [`declarations_from_ast`](../../crates/mununu-core/src/adapter/slang/mod.rs) + [`fill_declarations`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`SourceMapView`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API) — `mununu sv verify-auto` (text and `--json`) and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono, mununu-ui, and anyone who read the 0.5.7 legend in the `mununu-sva` image and saw `?` where a declaration line should be.

## TL;DR

- **The 0.5.7 legend's declarations were missing in the image.** The image's yosys-slang (oss-cad-suite 2026-08-24, yosys 0.68) stamps `src` on storage cells but not on nets, so every cell read `state_q ← ? (register, assigned at pump_pair.sv:52)`; the host's newer build stamps both, which is why the host measurement was complete. A measured surprise, fixed before the material was rewritten.
- **Declarations now come from slang's AST** (`--ast-json-source-info`, one extra slang run on the staged original files — the SVA extraction's own run works on a preprocessed copy whose lines are shifted), so they do not depend on the lift's front end; yosys's attribute still wins where the netlist carries it, the AST fills every gap. On the sv2v path the netlist's kinds are kept, its lines dropped, and the declarations come from the AST too.
- **`diagnostics.source_map.status` gains `partial`**: some cells placed, `reason` says what is missing (the sv2v path has no assignment lines; a cell the AST did not declare under the lift's name). `available` means every cell has its declaration and the lift stamped its `always` blocks; `unavailable` means nothing could be placed. The text prints `source map: partial — …` or `unavailable — …` under `model:`, and nothing when available.
- **No `?` in the text.** A half the map lacks is left out of the cell's line: `state_q (register, assigned at pump_pair.sv:52)`.
- Verdicts, exit codes, every other field: unchanged. The schema is regenerated for the doc change on `SourceMapView`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `trace.sources[].declared` is now present in the image; `diagnostics.source_map.status` may read `partial` | a gate that tested `status == "available"` should accept `partial` when it only needs declarations |
| **monono** | same; a lane preprocessing with sv2v sees `partial` with declarations and no assignment lines, instead of `unavailable` | none required |
| **mununu-ui** | the `status` enum gains a value | regenerate types; show `reason` on `partial` |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the second slang run and the fill live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | this is the image whose yosys-slang does not stamp nets |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `slang::declaration_tests::declarations_follow_the_instance_tree_with_the_lifts_spelling` (the AST shape slang 12.1 dumps with source info: top members bare, a bound instance under its name, a port once, a generate block as `$`) | pass |
| `verify_auto::source_map_tests::declarations_fill_the_gaps_and_settle_the_status` (a netlist gap filled by name; a cell the AST lacks → `partial` naming it; everything placed → available; sv2v → partial with its reason; nothing placed → unavailable) | pass |
| the drift suite after regeneration; `cargo check --workspace --all-targets`; clippy `-D warnings` on core + cli | pass / clean |
| the pump-pair draft on the host (yosys 0.70 + yosys-slang: nets stamped) through `--frontend slang`, `verilog`, `sv2v` | `slang` and `verilog`: the legend unchanged, `state_q ← pump_pair.sv:33 (register, assigned at pump_pair.sv:52)`, `flow ← pump_pair.sv:26 (input)`, no status line; `sv2v`: `source map: partial — the assignment lines are unavailable: sv2v preprocessed the source before yosys read it …`, `state_q ← pump_pair.sv:33 (register)`, `flow ← pump_pair.sv:26 (input)`, JSON `source_map: {status: "partial", reason}` |
| the pump-pair draft in the `mununu-sva` image (yosys 0.68 + yosys-slang: nets not stamped) — the run that found the defect, re-taken on the fix (the branch built in a scratch volume) | `state_q ← pump_pair.sv:33 (register, assigned at pump_pair.sv:52)`, `flow ← pump_pair.sv:26 (input)`; no status line (every cell placed) |

## Provenance

- The 0.5.7 re-run of the tutorial material in the image (2026-10-29 21:00Z): every legend line read `← ?`; the image's flat netlist JSON had `src` on 30 of 30 cells and 0 of 35 nets, the host's on both.
- The plan that scheduled the AST route for the sv2v path: `.claude/plans/rtl-source-map-2026-10-29.md` (private); it now serves every front end.

## Not covered here

- Assignment lines on the sv2v path (the AST's procedural blocks could give them; not done).
- The property's own line and the lines of its cone (the next PR).
