# Consumer briefing: a lift asked to show its tool run does not use the lift cache (mununu#1232, 2026-11)

> Source of truth: [`TOOL_RUN_OBSERVERS`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and `LiftCacheOutcome::Bypassed` in [`adapter/yosys/lift_cache.rs`](../../crates/mununu-core/src/adapter/yosys/lift_cache.rs) — surface: CLI+API — every `sv` verb and route that lifts SystemVerilog

**Audience:** anyone who lifts SystemVerilog with `MUNUNU_LIFT_CACHE_DIR` set (rosf and monono CI, `make e2e`) and debugs a lift with `MUNUNU_KEEP_YOSYS_TMP` or `MUNUNU_DEBUG_YOSYS_SCRIPT`.

## TL;DR

- **Before:** with a lift cache configured, a lift the cache already held ran no tool. Under `MUNUNU_KEEP_YOSYS_TMP=1` no staging directory was kept, and under `MUNUNU_DEBUG_YOSYS_SCRIPT` no script was printed. Nothing said why: the cache outcome is a log line at `info` (`lift cache hit`) and is in no report.
- **Now:** a lift under either variable does not use the cache. The tools run, the directory is kept or the script is printed, and no entry is read or written. One line is logged at `warn`: ``lift cache bypassed: `MUNUNU_KEEP_YOSYS_TMP` is set and shows a tool run, so the tools ran and the lift cache was neither read nor written; unset it to use the cache``.
- **No verdict, JSON field, flag or route changes.** A lift's outputs are the same with and without the cache; only whether the tools run for this lift changes, and only under the two variables.
- **Cost:** a lift under either variable takes the time of a cold lift, every time. Leave them unset in CI.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A job that sets `MUNUNU_KEEP_YOSYS_TMP` or `MUNUNU_DEBUG_YOSYS_SCRIPT` together with `MUNUNU_LIFT_CACHE_DIR` now lifts every design afresh and gets the directory or the script it asked for. A job that sets neither is unchanged. | None, unless a job exports one of the two by habit: remove it there to keep the cache's speed. |
| **monono** | Same. | Same. |
| **mununu-ui** | Nothing: no route, field or note changes. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the change. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

No cache directory needs clearing: the cache format and the keys are unchanged.

## Provenance

- **Found by:** the nightly image sweep of 2026-11-23 on `main`. The test `e2e_the_lift_runs_under_a_staging_root_with_a_space` lifts under `MUNUNU_KEEP_YOSYS_TMP=1` and looks for the kept directory. The sweep keeps a lift cache in its target volume between runs, so on every sweep after the first the lift was served from the cache and the test found no directory. It is a defect of the test's assumption and of the variable's behaviour under a cache; no verdict was wrong.
- **Tests:** `x1232_every_variable_that_shows_a_tool_run_bypasses_the_lift_cache` (over the table of such variables, no tool needed) and `e2e_x1232_a_lift_that_shows_its_tool_run_runs_the_tools_on_a_warm_cache` (the real lift in the image: a stored lift, then one lift under each variable runs the tools, then a plain lift is a hit again). The test that failed passes unchanged.
- **Class:** `cache-hit-observer` in [`docs/defect-classes.md`](../defect-classes.md).

## Not covered here

- The cache outcome (`hit`, `miss`, `bypassed`) is still only logged. No report carries it.
- The per-module lift (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`) does not use the lift cache, so nothing changes there.
- `MUNUNU_KEEP_VERILATOR_TMP` is about the Verilator work directories, which the lift cache does not hold; it is unchanged.
- A test that compares two lifts of one design is still served the first lift twice under the sweep's cache (mununu#1233).
