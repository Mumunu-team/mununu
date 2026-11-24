# Consumer briefing: two isolated exact-engine evaluations at once no longer share one model file (2026-11)

> Source of truth: [`exact_symbolic_with_exe`](../../crates/mununu-core/src/adapter/engine_isolate.rs#L120) and [`per_call_name`](../../crates/mununu-core/src/adapter/mod.rs#L1637) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` with `MUNUNU_ISOLATE_ENGINES` set; the routes that take `emit_testbench` with `replay: true`

**Audience:** anyone who sets `MUNUNU_ISOLATE_ENGINES` on a process that verifies more than one thing at a time (a `mununu server` that takes concurrent requests, a program that calls the library from several threads), and anyone who sends concurrent requests with `replay: true`.

## TL;DR

- **Only a process that runs two evaluations at once with `MUNUNU_ISOLATE_ENGINES` set is affected.** The variable is off by default; it runs the `exact-symbolic` engine's first pass in a child process. A command-line run is not affected: it gives its properties to this engine one at a time, under a portfolio too.
- **The child could be handed another evaluation's model (mununu#1221).** The parent passes the model to the child through a file, and the file's path was one per process. With two evaluations at once, the second could overwrite the file before the first one's child read it, and that child's verdict was then reported for the first evaluation: **a verdict computed on the wrong model**. Or the first to finish removed the file before the other child opened it, and that row was `unknown` with `bottom_reason.kind = "engine-crashed"`.
- **Each evaluation now writes a file of its own.** Nothing else changes in what the child computes or in how its answer is read.
- **If you ran with the variable set on a server that took concurrent `sv verify-auto` requests, treat the verdicts of overlapping requests as unconfirmed** and run them again. Requests that did not overlap, and every run without the variable, are not affected.
- **The replay directory of an HTTP request is also the request's own now.** With `replay: true`, the testbench of a property was built in a directory named by the server's process, the row's index and the property's name. Two requests at once that replayed a property of the same index and name built in the same directory, and the first to finish removed it. That one is read from the code, not reproduced.
- **The names of scratch files and directories changed.** Every file or directory a call makes for itself (the staged sources of a lift, a Verilator build, the model files handed to Pono and rIC3, a cache store's staging directory) is now named the same way: its former prefix, then the process id, a counter and the clock. A script that matched these by prefix still matches; one that parsed the rest of the name does not. They are removed when the call ends, as before, unless `MUNUNU_KEEP_YOSYS_TMP` or `MUNUNU_KEEP_VERILATOR_TMP` keeps them.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | With `MUNUNU_ISOLATE_ENGINES` on a process that verifies concurrently: verdicts of overlapping evaluations are now each computed on their own model. | If a lane ran that way, re-run the overlapping requests. Otherwise none. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: `sv verify-auto` is not a CTXDSL capability. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Fix:** pending merge — see branch `fix/isolated-model-per-call-1221`. Issue: [mununu#1221](https://github.com/Mumunu-team/mununu/issues/1221). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- **Found by:** reading `adapter/engine_isolate.rs` while fixing mununu#1196. Reproduced with stand-in children, not with the engine: four evaluations at once, each child a script that reports the first line of the model file it was handed. Before the fix a child reported another evaluation's model; after it each reports its own.
- **engine:** `exact-symbolic` (full-state ROBDD / OxiDD, bit-blast μ-fixpoint), run in a child process; role: first-pass decider of `sv verify-auto`. The change is in how the parent hands the model over, not in the engine.
- **Class:** `scratch-name-per-call` in [`docs/defect-classes.md`](../defect-classes.md), which holds the case table.
- **Tests:** `x1221_two_isolated_evaluations_at_once_each_get_their_own_model` (the reproducer), `x1221_names_asked_for_at_once_are_all_different` (the one function that makes the names), and in `tests/policy` the four tests of `scratch_names_are_per_call_1221` (every site that names such a file or directory, and none that builds a name from the process id by itself).
- **API schemas:** no shape change.

## Not covered here

- **No report says that two evaluations shared a file in the past.** A verdict that was computed on the wrong model carries nothing that marks it; only the overlap of the two evaluations in time tells.
- **Two server processes** never shared the file: the path carried the process id before the fix too.
- **Under isolation the row gets the first pass only**, as before.
