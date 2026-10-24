# Consumer briefing — `sv verify-auto --emit-testbench`: a Verilator replay of every counterexample (2026-10)

> Source of truth: [`counterexample_testbench`](../../crates/mununu-core/src/adapter/verilator/replay.rs#L69) — surface: (CLI+API) — `sv verify-auto --emit-testbench DIR [--tb-clock PORT]` / `POST /api/v1/sv/verify-auto` with `emit_testbench: true`

**Audience:** rosf, monono, mununu-ui and anyone who consumes the `sv verify-auto` report or validates its counterexamples under Verilator (the target-executor Phase 3.5 procedure). P0.7 of the EmTech tutorial plan.

## TL;DR

- **New CLI flags.** `--emit-testbench DIR` writes `DIR/<property>/{tb.cpp, Makefile}` for every row that carries a counterexample; `--tb-clock PORT` names the clock when no port is called `clk`, `clk_i`, `clock`, `i_clk`, `aclk` or `clk_sys`. The JSON report is unchanged when the flag is absent.
- **New API fields.** The request gains `emit_testbench` (bool, default `false`) and `tb_clock` (optional string); each property row gains `testbench` (`{tb_cpp, makefile, checks, skipped}`), `null` unless requested and a counterexample exists. Both schemas are regenerated (`sv-verify-auto-request.schema.json`, `sv-verify-auto-response.schema.json`); the additions are optional, so existing parsers are unaffected.
- **No verdict changes.** The testbench reads the trace the engine already returned; no outcome, message or exit code moves.
- **What the testbench does.** It resets for one cycle, drives the trace's inputs, compares every design register the trace names and prints `MATCH` or the first `MISMATCH`, then clocks once more so the bound SVA, compiled with `--assert`, fires. Synthetic cells (`__past`, inputs-in-the-state, monitor latches) are reported as `skipped`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | nothing unless the flag is passed; a new optional `testbench` field may appear in rows | none; optionally adopt the flag for counterexample validation instead of a hand-written harness |
| **monono** | same | none |
| **mununu-ui** | the response type gains an optional field | none required (RTL verb, no UI affordance per the surface-parity rule) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and want the flag | the flag lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes, to run the emitted Makefile beside the binary | Verilator is in this image |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not decide verdicts |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | additive surface, not used by rosf |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | additive surface, not used by monono |

## Verification steps for every consumer

```bash
mununu sv verify-auto rod.sv --source rod_sva.sv --top rod --frontend slang --emit-testbench tb
make -C tb/p_no_fault      # in mununu-sva: expect MATCH, then the SVA assertion failure
```

## Provenance

- Branch `feat/emit-testbench`; measured in `mununu-sva` on a two-register design with a bound assertion: `MATCH`, then `Assertion failed in TOP.rod.u_sva.p_no_fault`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- `--emit-witness` (a btorsim witness plus a yosys-sim VCD) is a separate change.
- A liveness lasso is replayed as its prefix followed by one pass of its loop: the testbench checks those states, but a finite simulation cannot by itself show that the loop never progresses. Two-player strategies are not replayed.
