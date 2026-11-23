# Consumer briefing: the isolated exact-engine child is ended at the property's budget (2026-11)

> Source of truth: [`exact_symbolic_with_exe`](../../crates/mununu-core/src/adapter/engine_isolate.rs#L120) and [`run_to_end`](../../crates/mununu-core/src/adapter/mod.rs#L1282) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` with `MUNUNU_ISOLATE_ENGINES` set

**Audience:** anyone who sets `MUNUNU_ISOLATE_ENGINES`, and anyone who runs `mununu server` and cares what a request leaves running after its client has gone.

## TL;DR

- **Only runs with `MUNUNU_ISOLATE_ENGINES` set see a different row.** The variable is off by default; it runs the `exact-symbolic` engine's first pass in a child process, so a crash of the engine is one `unknown` row and not a lost run.
- **The child had no stop (mununu#1196).** The parent waited for it to exit, whatever `MUNUNU_PROPERTY_BUDGET_MS` and `MUNUNU_VERIFY_BUDGET_MS` said, and the child's own verb enters neither budget. On a design the engine cannot finish, the row never returned; an HTTP request that timed out left the child running.
- **The child is now given the time the property's budget has left, and is ended when it is spent.** The row is `unknown` with `bottom_reason.kind = "budget-expired"`, and a `time-budget-exceeded` note carries the property's name and the time the child was given. `--fail-on unknown` sees it.
- **A cancelled run ends the child too**: a request past `--request-timeout`, or one whose client disconnected.
- **A budget already spent, or a run already cancelled, starts no child.** The row and the note are the same, and the note says the child was not started.
- **A stop is no longer reported as a crash.** Before, the only rows a child that gave no verdict could produce were `engine-crashed` with an `engine-isolation` note. Those still mean the child died: its exit code or signal is in the note.
- **No change** to a verdict the child reaches inside its budget, to a run with both budgets turned off (`0`), or to any run without the variable.
- **Other children a cancelled run now ends:** slang, yosys and sv2v when `MUNUNU_LIFT_TIMEOUT_MS=0` turned their wall off, the Verilator build, the reset-simulation binary, `make` for the replay testbench, clang for `codesign extract-c`, and the `git rev-parse` of the extraction spec's commit check. Their results are unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | With `MUNUNU_ISOLATE_ENGINES`: a row that used to hang now returns `unknown` / `budget-expired` at the property's budget. A gate that counts `engine-crashed` rows no longer has to tell a slow design from a dead engine. | Branch on `bottom_reason.kind`. To give the exact engine longer, raise `MUNUNU_PROPERTY_BUDGET_MS`. |
| **monono** | Same. A lane that sets the variable to survive engine aborts keeps that; it also ends on time. | Same. |
| **mununu-ui** | None: `sv verify-auto` is not a CTXDSL capability. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the review of the solver-child fix (mununu#1186), which listed the isolated child as the one spawn outside the shared waiting helper and filed it as mununu#1196. Read from the code; the reproducer below is the first run of it.
- **Class:** `abandoned-request-work` in [`docs/defect-classes.md`](../defect-classes.md), which holds the case table.
- **Tests:** `x1196_an_isolated_child_past_the_property_s_budget_is_ended_and_reported_as_a_stop` (the reproducer: a stand-in child that does not exit, under a property budget), `x1196_a_cancelled_run_ends_the_isolated_child`, `x1196_a_spent_budget_or_a_cancelled_run_starts_no_child`, `x1196_a_child_that_answers_inside_its_budget_is_read_as_it_answered`, `x1196_every_way_an_isolated_child_ends_has_a_row` (what each way the child ends puts in the report), and the enumerating `policy::child_waits_go_through_one_helper_1196` (every file that starts a child process, and the helper call that waits on it).
- **API schemas:** no shape change. `budget-expired` and `time-budget-exceeded` are existing values.

## Not covered here

- **The child stops because the parent ends it.** The child's verb enters neither wall-clock budget, so a child whose parent is killed outright (`SIGKILL`) runs until the engine finishes.
- **The stop is a wall clock**, so whether a row is decided or `unknown` under isolation depends on the host, as it does for every budget stop.
- **Under isolation the row gets the first pass only**, as before: a row the child does not decide is not handed to the in-process ladder in that pass.
- **Two isolated evaluations in one process share one model file** (mununu#1221). A server that sets the variable and serves two `sv verify-auto` requests at once is exposed; one CLI run is not.
- **When isolation cannot be attempted** (the executable cannot be found, the model file cannot be written), the engine runs in the parent process and the report does not say so (mununu#1222).
- **The Verilator build, the simulation and replay runs, clang and git have no wall** (mununu#1223). A cancelled run ends them; a run nobody cancels waits for them.
