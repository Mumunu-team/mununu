# Consumer briefing: the path work after a verdict has a limit of its own, and says when it stops (2026-11)

> Source of truth: [`safety_path_under_slice`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L1899) — surface: CLI+API — the `trace` and the notes of a violated row of `sv verify-auto`, and `trace_shortest` of `btor2 verify --fault … --target`

**Audience:** anyone who reads the counterexample of a violated `sv verify-auto` row or its `witness-unavailable` notes, runs `sv verify-auto` under a wall-clock budget, or reads the attack of `btor2 verify --fault … --target`.

## TL;DR

- **What changes is the work after the verdicts (mununu#1257), with one exception that touches a verdict.** The work is the search for the path that explains a `violated`, and the search for the first such path. The exception: that search is also a cross-check. A member that proves the violation unreachable during it turns the row from `violated` to `unknown` with `bottom_reason: engine-contradiction` (mununu#1076). The cross-check now stops at the row's slice with the search, so a contradiction that a member would reach only after the slice is not found: the row stays `violated`, with a `witness-unavailable` note (stage `search`). The re-plan of undecided rows already ran this check under the slice, and a run whose budget was spent skipped it; what is new is the default for rows the first pass decided. `MUNUNU_RESCUE_SLICE_MS=0` restores the earlier bound.
- **Each violated row's path search now runs under a slice of its own.** The slice is `MUNUNU_RESCUE_SLICE_MS` (60 s by default), per row. Before, the search ran under what was left of the run's budget, one hour by default. On the in-repo example `examples/verify/v13_video_timing_raster_twin` (measured 2026-11-24, debug build of `c14978e1` in the `mununu-sva` image) the eight verdicts took about one second, and the run then went on to the end of its budget on the path work of the first violated row; under a run budget of 100 s and of 600 s the second violated row shipped with no path and no note.
- **A path that needs more than the slice no longer ships by default.** The row keeps its verdict and has a `witness-unavailable` note with stage `search`, closing with the setting to raise. To get the earlier behaviour set `MUNUNU_RESCUE_SLICE_MS=0` (the run's budget alone bounds the search) or give the slice more time. The same variable bounds each step of the re-plan of undecided rows, so a larger value also gives those steps more time.
- **A row whose path search already missed is searched once.** A row the re-plan decided `violated` and could not find a path for, under the slice, was searched a second time by the pass that runs after every verdict, and could get a second `witness-unavailable` note. It now keeps its one note.
- **Every violated row leaves with a path or a note.** Two stages are new in `witness-unavailable` (`items: [stage]`):

  | stage | when | what the reason says |
  |---|---|---|
  | `budget` | the row's turn came with the run's time spent, the run cancelled, or the memory ceiling reached | which of the three; for time, the setting that ended the run. Before: no note, for this row and every later one |
  | `parse` | the row's own formula does not parse back | that this is a mununu defect, with where to report it. Before: no note |

- **New note kind `witness-not-shortest`** (level info, `property` set, `items: [member]`). A member's path ships once it is confirmed on the concrete simulator. When the member does not establish that no shorter path exists, native BMC looks for the first one, one query per depth up to the path's. When that search stops, the member's path still ships and this note says the path is a counterexample, not established the shortest, and why the search stopped. Before, that was a log line, and the search had no limit of its own.
- **`btor2 verify --fault … --target` says whether its attack is the shallowest.** `trace_shortest` was always absent there; it is now `true` or `false`. With `false`, the new field `trace_not_shortest_reason` (string) says why the first attack was not re-derived. The re-derivation runs under `--timeout-ms` / `timeout_ms` when one is given and under the rescue slice otherwise; before, without a timeout it had no limit. The CLI JSON has both keys (`null` when unset); the API omits them when unset. Without `--target`, `trace_shortest` reports what the member established, and there is no reason field to read.
- **`--localize`:** a violated row whose formula does not parse back now has `trace.localization.status: "unavailable"` with the reason. Before, it had no status.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A violated `sv verify-auto` row whose path needs more than 60 s of search ships without a `trace` and with a `witness-unavailable` note (stage `search`); the cross-check of that row by the path search stops there too. A run that ends on its budget has a note on every violated row without a path. New note kind `witness-not-shortest`. | If a lane needs deep paths, or treats a `violated` with a `witness-unavailable` note of stage `search` as not yet cross-checked, set `MUNUNU_RESCUE_SLICE_MS`. A reader that lists note kinds or `witness-unavailable` stages should accept the new ones. |
| **monono** | Same. A `make formal` run on a design with a deep violation ends minutes after its verdicts, not at the run budget. | Same. |
| **mununu-ui** | No UI affordance for the RTL verbs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the slice and the notes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use, or do not verify. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | When the lane wants the bounded path work: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** mununu#1257, from one end-to-end test that took an hour in every image sweep while its verdicts took a second.
- **Tests:** `every_violated_row_a_spent_run_cannot_search_names_the_setting_that_ended_it` (every setter of a budget), `each_violated_row_is_searched_under_its_own_slice`, `a_row_whose_search_already_missed_is_not_searched_again`, `a_search_that_stops_at_its_slice_closes_with_the_slice_remedy`, `a_path_not_established_shortest_has_a_note_of_its_own_kind`, `x1257_a_rederivation_with_no_deadline_runs_under_the_rescue_slice`, and the policy test `post_verdict_work_is_bounded_1257`, which holds a row for every pass that runs on a decided report.
- **Class:** `post-verdict-work-budget` in [`docs/defect-classes.md`](../defect-classes.md).

## Not covered here

- The size of a report that carries a very long path (mununu#1258).
- A `witness-unavailable` reason of `sv verify-auto` can end "(raise --witness-max-k)", a flag that verb does not have (mununu#1259).
- `--localize` has no limit per row beyond the run's budget (mununu#1260).
- The path of an `AG EF` trap certificate: a miss, and a path not established the shortest, reach only the log (mununu#1261).
- The stop texts of the single members that have no closing clause (mununu#1243).
- The antecedent-vacuity check of the fault posture has no test of its stop, and its note names no setting (mununu#1264).
