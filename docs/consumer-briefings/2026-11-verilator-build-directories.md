# Consumer briefing: a Verilator build that cannot run where it was asked to says so (2026-11)

> Source of truth: [`build_dir_refusal`](../../crates/mununu-core/src/adapter/verilator/mod.rs#L366) and [`bench_refusal`](../../crates/mununu-core/src/adapter/verilator/replay.rs#L175) — surface: CLI+API — `sv verify-auto --emit-testbench DIR [--replay]` / `POST /api/v1/sv/verify-auto` with `emit_testbench: true` (and `replay: true`), and the reset simulation that seeds `sv verify-auto`

**Audience:** anyone who replays a counterexample under Verilator (`--emit-testbench`, `--replay`, `emit_testbench` / `replay` over HTTP), who runs mununu with a `TMPDIR` or a source tree whose path has a space, or who matches on the `replay testbench:` lines of the CLI.

## TL;DR

- **What it is.** Verilator builds through `make`, and its makefile refuses to build in a directory whose full path has whitespace; a word of a `Makefile` cannot carry whitespace either. mununu put its Verilator work directories under the system temporary directory whatever `MUNUNU_LIFT_STAGING_ROOT` said, and wrote source paths into the bench's `Makefile` as they came. With a space in any of those, the user got the tail of `make`'s output, or — for a source path — a report with no testbench and the line `no counterexample with a path to replay`, which was false (mununu#1176).
- **No verdict changes.** Only the Verilator cross-check and what is said about it.
- **`MUNUNU_LIFT_STAGING_ROOT` now also moves the Verilator work directories** (the reset simulation's, and the HTTP `replay: true` scratch directory). With a root that has whitespace (the variable, or the system temporary directory when it is unset) those two steps are skipped, and the message names the variable.
- **`--emit-testbench DIR` with whitespace in DIR's full path** (links resolved): the files are still written. With `--replay` the row says `replay: unavailable — Verilator's make build does not run in a directory whose path has whitespace (…) — a limit of Verilator's build, not of the design; pass --emit-testbench a directory whose full path has none`. Without `--replay`, one line on stderr says `make` will not build there.
- **A source path the bench's `Makefile` cannot name** (anything outside ASCII letters, digits and `_` `-` `.` `/`): no testbench is written for that run. The text report prints `replay testbench: not written — <why>` under each row with a counterexample, stderr counts them, and the JSON row has the new optional field `trace.replay.not_generated` (a string). With `--replay` / `replay: true`, `trace.replay.result.outcome` is `unavailable` with the same reason.
- **The stderr line `replay testbench: no counterexample with a path to replay`** is now printed only when that is the case.
- **`MUNUNU_VERILATOR_PATH` with a space** works: the generated `Makefile` runs `"$(VERILATOR)"` as one word.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | New optional `trace.replay.not_generated`; `trace.replay` can be present without `testbench`. `replay: true` on a server whose temporary directory has whitespace returns `unavailable` naming `MUNUNU_LIFT_STAGING_ROOT`. | Read `not_generated` before `testbench`. Name `source_name` and `additional_sources[].name` with ASCII letters, digits and `_` `-` `.` `/`. |
| **monono** | Same; a lane that runs `--emit-testbench` under a path with a space gets `unavailable` with the reason, not a `make` error. | Point `--emit-testbench` at a directory without whitespace. |
| **mununu-ui** | `sv verify-auto` is not a CTXDSL capability; the field arrives through the generic row. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the messages and the field. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the cross-review of the yosys staging fix (mununu#1138), which filed the Verilator directories as mununu#1176.
- **Class:** `lift-script-path` in [`docs/defect-classes.md`](../defect-classes.md), which holds the case table.
- **Tests:** `x1176_a_verilator_work_directory_is_never_made_under_a_path_with_whitespace` (the reproducer), `x1176_the_staging_root_variable_moves_the_verilator_work_directory`, `x1176_a_build_directory_is_refused_for_whitespace_anywhere_in_its_full_path`, `x1176_a_replay_in_a_directory_with_whitespace_is_refused_before_make_starts`, `x1176_a_design_the_makefile_cannot_name_is_refused_with_its_reason`, `x1176_the_makefile_runs_the_verilator_binary_as_one_word`, the enumerating `policy::verilator_build_directories_1176` (every `make` spawn and every chooser of a build directory under `adapter/verilator/`), and the image's `e2e_compile_verilator_takes_a_source_under_a_directory_with_whitespace`.
- **API schemas:** `docs/api-schemas/` regenerated for `trace.replay.not_generated`.
- **Measured in the `mununu-sva` image:** on a 4-bit counter with one violated assertion, Verilator 5.051 —
  - control: `replay: match — 0 of 4 checks disagree`;
  - `--emit-testbench "<dir with a space>/tb" --replay`: `Makefile`, `tb.cpp` and `replay.gtkw` written, the row says `replay: unavailable — Verilator's make build does not run in a directory whose path has whitespace (…) — a limit of Verilator's build, not of the design; pass --emit-testbench a directory whose full path has none`; without `--replay`, stderr says `replay testbench: make will not build there — …`;
  - the source under a directory with a space: no directory is made, stderr says `replay testbench: not written for 1 counterexample(s) — the bench's Makefile takes a source path made of ASCII letters, digits and _ - . / only, and this one is not: … — copy the sources under a path with only those characters and run again (a link is not enough: the path is resolved before it is written)`, and the JSON row has `trace.replay.not_generated` with `result.outcome: "unavailable"`;
  - `MUNUNU_VERILATOR_PATH` pointing into a directory with a space: `replay: match — 0 of 4 checks disagree`;
  - a `TMPDIR` with a space: the reset simulation's tests fail with the refusal naming `MUNUNU_LIFT_STAGING_ROOT`, and pass once that variable points at a plain directory;
  - a SystemVerilog source under a directory with a space, compiled by the reset simulation: builds and runs (the path is an argument there, not a `Makefile` word).

## Not covered here

- Verilator's own limit stays: a build directory with whitespace cannot be used, and mununu does not copy the bench elsewhere to get around it.
- The other temporary files mununu makes (the slang extraction's, the pono and rIC3 members' model files, the isolated-engine child's directory) stay under the system temporary directory: each is an argument of a tool, and under a `TMPDIR` with a space `sv verify-auto` and `btor2 verify` gave the same results as under a plain one, every member answering.
