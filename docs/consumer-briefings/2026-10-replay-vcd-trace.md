# Consumer briefing — the generated replay testbench gains `make trace` (a VCD for GTKWave) (2026-10)

> Source of truth: [`counterexample_testbench`](../../crates/mununu-core/src/adapter/verilator/replay.rs) — surface: (CLI+API) — `sv verify-auto --emit-testbench DIR` and `emit_testbench: true` on `POST /api/v1/sv/verify-auto`.

**Audience:** monono (runs the generated `make` in its replay lane), rosf, and anyone who opens a counterexample in a waveform viewer.

## TL;DR

- **Additive.** The generated `Makefile` has a new target, `trace`: the same testbench built with Verilator `--trace` and `-DMUNUNU_TRACE`, in `obj_dir_trace/`, which runs the replay and writes `replay.vcd` next to `tb.cpp` (two time units per cycle, the reset cycle first). `make` (the default `run` target) is byte-for-byte the same build as before and does not need the VCD library.
- `tb.cpp` carries the tracing code under `#ifdef MUNUNU_TRACE`; without the define it compiles exactly as before (an empty `dump()`).
- `make clean` now also removes `obj_dir_trace` and `replay.vcd`.
- JSON shape (`trace.replay.testbench` carries the same two files), flags, exit codes: unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | nothing in the replay lane (`make` unchanged); `make trace` is available for a waveform | none required |
| **rosf** | same | none |
| **mununu-ui** | nothing (RTL verbs) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the generator lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | where `make trace` runs (Verilator and GTKWave ship there) |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `replay::tests::the_makefile_has_a_trace_target_and_the_testbench_dumps_a_vcd_under_it` | MEASURED_UNIT |
| `make trace` on a generated testbench in `mununu-sva` (the tutorial's `pump_seq` draft counterexample): build, `MATCH`, `replay.vcd` written with the design's signals | MEASURED_IMAGE |

## Provenance

PROVENANCE_PLACEHOLDER

## Not covered here

`--emit-witness` (a VCD from the engine's own trace, via btorsim) remains unbuilt; this target replays on the RTL under Verilator, which is the cross-check the claims policy asks for.
