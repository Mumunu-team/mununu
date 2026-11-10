# Consumer briefing: a budget that stops the ⊥ escalation is named on the row (2026-11)

> Source of truth: [`escalation_stopped_note`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L9802) — surface: CLI+API — `sv verify-auto --json` / `POST /api/v1/sv/verify-auto` (`notes[]`); no UI affordance.

**Audience:** rosf, monono, mununu-ui, and anyone whose gate reads `sv verify-auto` notes.

## TL;DR

- **New notes only. No verdict changes.**
- When the process memory ceiling (`MUNUNU_MAX_PROCESS_MEMORY_BYTES`) or the run's wall-clock budget stops one of verify-auto's later passes, a note now says so. The passes are the ⊥ escalation and the exact rescue of skipped rows.
- **Note shape:**
  - `kind` is `memory-budget-exceeded` or `time-budget-exceeded`, the same kinds the run-level notes use;
  - `level` is `scope-caveat`;
  - `property` is the first row the pass did not attempt.
- **Before:** those passes stopped with a bare `break`. The row kept its `unknown` / `skipped`, and its only note was the generic `bottom-reason` ("residual ⊥ was not classified … most commonly a resource abstain on the exact-symbolic or cube engine"), which blamed an engine for a stop that was a budget.
- A run whose main loop already hit the budget still has its run-level note (`property: null`). It may now carry a row-scoped one as well.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | A gate counting `memory-budget-exceeded` / `time-budget-exceeded` notes may see one more, carrying `property`. | Key on `kind`, not on the count. No action if the gate only tests for presence. |
| **rosf** | Same. | None required. |
| **mununu-ui** | No UI affordance. The note list may include the new rows. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Yes, if consumers pin a version tag and want the notes. | The notes come from the binary. |
| `mununu/docker/Dockerfile.dev` | Only if the dev workflow reads the notes. | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, if you run `sv verify-auto` in the image. | The binary on the volume changes. The image's toolchain does not. |
| `mununu/docker/Dockerfile.profile` | No. | Profiling image. |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | No. | Extraction images do not verify. |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | Yes, if rosf pins a mununu version and wants the notes. | Picks up the change. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Yes, if monono pins a mununu version and wants the notes. | Picks up the change. |

## Provenance

- **mununu#871.** A lib test flaked: a fair-cycle row came back `unknown` under parallel load.
  - Cause: a sibling test set `MUNUNU_MAX_PROCESS_MEMORY_BYTES=1073741824` process-wide, while the suite's RSS peaks at about 4.4 GiB. A test that reached a memory checkpoint inside that window abstained.
  - The failing row's only note blamed an unclassified engine abstain, so the stop was silent.
  - The test no longer writes the environment, and the stop now reaches the report.
- **Tests:**
  - `an_escalation_stopped_by_the_budget_names_the_row_it_skipped`: an expired run budget gives a row-scoped `time-budget-exceeded`. It fails with only `bottom-reason` when the note push is removed.
  - `env_var_values_resolve_through_the_four_cases` and `the_env_path_agrees_with_the_pure_resolution`.
  - `no_new_lib_code_writes_the_process_environment`.

## Not covered here

- mununu#880: the other lib tests that write the process environment (tool paths such as `MUNUNU_CVC5_PATH`, and the partial-JSON and shadow-dump paths) are enumerated by `no_new_lib_code_writes_the_process_environment` but not removed. Each one is still a window in which a concurrent test sees a missing tool.
