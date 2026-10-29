# Consumer briefing — `--replay` runs the Verilator testbench from mununu; `replay.gtkw` opens the VCD on the trace's signals (2026-10)

> Source of truth: [`run_replay`](../../crates/mununu-core/src/adapter/verilator/replay.rs) + [`gtkwave_save`](../../crates/mununu-core/src/adapter/verilator/replay.rs) + [`ReplayResultView`](../../crates/mununu-core/src/trace.rs) — surface: (CLI+API) — `mununu sv verify-auto --emit-testbench DIR --replay` and `POST /api/v1/sv/verify-auto` with `emit_testbench: true, replay: true`.

**Audience:** rosf, monono, mununu-ui, and anyone who replays a counterexample under Verilator or opens its VCD.

## TL;DR

- **New flag `--replay` (API `replay: true`), requires the testbench.** mununu runs `make` and `make trace` in each emitted `DIR/<property>/` and reports the verdict on the row: text lines `replay dir:`, `replay:` (`match` / `mismatch` with the first disagreeing comparison / `unavailable — <reason>`), `replay assertion:` and `replay vcd:`; JSON `trace.replay.result` with `outcome`, `reason`, `checks`, `mismatches`, `first_mismatch`, `assertions`, `vcd_bytes`, `verilator`, `log`. Over HTTP the bench runs in a scratch directory on the server; the response carries the result, not the directory.
- **New file `replay.gtkw`, always written beside the testbench** (and returned as `trace.replay.gtkw`): a GTKWave save file listing the clock, the trace's inputs and the registers it names, in trace order, vectors in decimal. `gtkwave replay.vcd replay.gtkw` opens on the scene.
- **`trace.replay` gains three keys** (`gtkw`, `dir`, `result`), all absent when not produced; `testbench` is unchanged. The CLI's `--json` now carries `trace.replay` too (it did not before), so one document reads the same on both surfaces, plus `dir` on the CLI.
- **Silence is a defect:** a replay that cannot run (Verilator or `make` missing, a compile error) is `outcome: "unavailable"` with its `reason`, on the row.
- Verdicts, exit codes, the testbench files and every other field: unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | nothing unless you ask for `--replay`; `trace.replay.gtkw` appears whenever you emit a testbench | none required; `--replay` turns the manual `make` into a row field |
| **monono** | same; the replay `log` and `assertions` are the lines you used to read from `make` | none required |
| **mununu-ui** | `TraceReplay` gains `gtkw?`, `dir?`, `result?`; the runner can offer the `.gtkw` for download and show `result.outcome` | type the three keys (optional); a follow-up PR |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the flag and the files live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the image has Verilator and `make`; `--replay` runs there |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `replay::run_tests::the_bench_output_parses_into_verdict_mismatch_assertion_and_vcd` (MATCH + assertion + VCD line; MISMATCH (2) with its first cycle line; a compile error with no verdict) | pass |
| `replay::run_tests::the_gtkwave_save_file_lists_the_traces_signals_in_order` (clock, inputs, registers; ranges and radix; monitor cells left out) | pass |
| `api_schema_drift_sv_verify_auto_request` / `_response` and every other schema that embeds `Trace` (the request's `replay`, the trace's `replay` object) | pass after regeneration; `cargo check --workspace --all-targets` and clippy `-D warnings` on core + cli clean |
| `mununu sv verify-auto … --emit-testbench --tb-clock clk --replay` on the tutorial's pump-pair draft (private material; host build, the YosysHQ bundle's yosys 0.70 + yosys-slang + Verilator 5.053, GNU make 3.81; 28 s including two Verilator builds) | the violated `alternates` row prints `replay dir: …/alternates (16 checks, 3 cell(s) not compared); open: gtkwave replay.vcd replay.gtkw`, `replay: match — 0 of 16 checks disagree; verilator 5.053`, `replay vcd: …/alternates/replay.vcd (1983 bytes)`; no assertion line (the violated property is an annotation, the bound SVA holds); the directory holds `tb.cpp`, `Makefile`, `replay.gtkw` (680 bytes), `replay.log` (everything make printed) and `replay.vcd`; verdicts and exit code 2 unchanged |
| the same with `--json`: `trace.replay.{testbench, gtkw, dir, result}` on the violated row | present: `result = {outcome: "match", checks: 16, mismatches: 0, assertions: [], vcd_bytes: 1983, verilator: "5.053", log: "MATCH\nwrote replay.vcd"}`; `reason` and `first_mismatch` absent, not null |

## Provenance

- Review of the tutorial videos' run scripts (2026-10-29): the replay was the step the scripts ran by hand (`make`, `make trace`, then checking the VCD existed and finding the emitted directory); the GTKWave signal list was added by hand before each recording. Both are now mununu's.
- The testbench emitter and its VCD target: [`2026-10-emit-testbench.md`](2026-10-emit-testbench.md) and [`2026-10-replay-vcd-trace.md`](2026-10-replay-vcd-trace.md); the user-facing section is `docs/verifying-rtl.md` §"Replaying a counterexample under Verilator"; the trace shape: `docs/api-schemas/verdict.md` §Trace.

## Not covered here

- The replay does not re-decide anything: it is the cross-check the claims policy asks for, and its verdict sits beside the engine's, never replaces it.
- `replay.gtkw` names signals under `TOP.<top>.`; a design whose top is instantiated under another name in the bench (never the case for the generated bench) would need its own save file.
- Running the replay on a design whose sources the Makefile cannot find on the server (HTTP: only the request's `source` and `additional_sources` are staged) reports `unavailable` with Verilator's message.
