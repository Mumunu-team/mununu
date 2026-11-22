# Consumer briefing — `trace_shortest`: the report says whether a counterexample is the first one (mununu#1080, mununu#1057)

> **Audience:** maintainers of the repositories that read a `violated` verdict's counterexample from `mununu btor2 verify --witness`, the HTTP route behind it, or `mununu sv verify-auto`'s slice and `--localize` (rosf, monono, mununu-ui). Not a release note.
>
> Source of truth: [`CarriedTrace`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L139) · [`SatBmcOutcome`](../../crates/mununu-core/src/adapter/btor2/sat_bmc.rs#L84) · [`Btor2VerifyResponse::trace_shortest`](../../crates/mununu-core/src/api/models.rs#L1166) · [`shortest_path_for`](../../crates/mununu-core/src/adapter/btor2/witness.rs#L700) — surface: CLI+API

## TL;DR

Two things change, one reverts and one is new.

1. **The `sat-bmc` in-place shrink of #1068 is reverted** (mununu#1080). It re-asked, after every hit, for a `bad` at a shallower depth under the same conflict budget until UNSAT. On the HWMCC'20 M2 set the shrink ran out of its budget on every run measured (30 / 30 in the A/B), refuted nothing, and shipped the counterexample it already had: the same trace at 2.1–5.1× the wall on the 16 rows `sat-bmc` decides (`shift_register_top_w16_d32_e0` 57 s → 292 s against a 300 s budget). The member's search is the one before #1068 again: one query per doubling window, the model's earliest true `bad` as the depth.
2. **The report says what the carrier established.** `btor2 verify --witness` (CLI JSON and `POST /api/v1/btor2/verify`) gains `trace_shortest`, beside `trace_by`:
   - `true` — the member refuted every depth below the trace's: `native-bmc` (the re-derivation, one query per depth) and `sat-bmc` when the hit sits at its window's base (always for its plain, per-depth search).
   - `false` — the member did not: `sat-bmc`'s dynamic step answered a window with a deeper path, and a shorter counterexample may exist inside that window (the slice fixture's 4-step violation shipped as 7 steps this way, mununu#1057).
   - absent — the member does not say (`ic3`, `btormc`, `ric3`, `ric3-bmc`: their own paths, never claimed shortest), or there is no trace.

   A consumer that reads the trace cycle by cycle (a slice, a localisation, a "first cycle that…" view) treats anything but `true` as *a* path to the violation, not the first one. The verdict, every other field and the routes are unchanged. Every carried trace is still replayed on the concrete simulator before it ships.

## The slice and `--localize` read a shortest path again (mununu#1057)

The consumers that read a trace cycle by cycle do not get the field; they get the path. `sv verify-auto`'s safety witness (the slice, `--localize`) and the fault adversary's attack search go through [`shortest_path_for`](../../crates/mununu-core/src/adapter/btor2/witness.rs): a shipped path whose carrier did not establish it is the shortest is re-derived by native BMC bounded by the path's own depth — one query per depth, so the first path it finds is the first one, and the bound guarantees one exists at or below it. A re-derivation that misses (its budget, an encode failure) keeps the carried path and logs it; the verdict is never touched. The cost is one bounded native-BMC search per violated row on that path, only when the carrier is not `sat-bmc` at its window base or `native-bmc` itself. The two e2e detectors on the slice fixture (`e2e_the_slice_of_a_counterexample_names_the_lines_that_decided_it`, `e2e_localize_names_the_state_register_at_the_second_grant_first`) are the acceptance test and pass again.

`btor2 verify --witness` does not re-derive: it reports `trace_shortest` and ships the carried path (a deep HWMCC counterexample re-derived by native BMC would cost minutes for no verdict change).

## Per consumer

### rosf · monono (CLI and HTTP)

- `btor2 verify --witness` and `POST /api/v1/btor2/verify` (with `witness`): a new optional field `trace_shortest: bool`, present only with a trace and only when the member says. `docs/api-schemas/btor2-verify-response.schema.json` is updated; the drift test pins it.
- A trace carried by `sat-bmc` may again be deeper than the one the #1068 build shipped for the same design (the 7-step path instead of the 4-step one on the slice fixture); a consumer that re-baselined trace lengths after #1068 re-baselines again, and reads `trace_shortest` from now on.
- `sat-bmc` wall time on counterexample rows returns to the N5.6 level (the 16 rows above: 2.1–5.1× faster than #1068).

### mununu-ui

No CTXDSL surface; no change required. A view that shows a counterexample's length may show `trace_shortest` next to it.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the member's search and the new field |
| `mununu-sva` (dev / e2e) | yes | same |
| consumers' own images | no | the field is optional; a consumer that reads it rebuilds on its own schedule |

## Provenance

A/B 2026-11-18 in `mununu-sva` on the box: three builds (`8b3b01f7` before #1068, `a58b464c` #1068, `6831311e` main), three of the slowed rows, two repeats each, default members and `MUNUNU_REACH_IC3=0`: `pre` 1.2–2.9 s, #1068 6.4–10.2 s, main 6.7–13.5 s, `shrink out of budget` on every post-#1068 run; the full owned-only M2 re-measurement on `6831311e` (51 / 68, the same verdicts as N5.6) is where the 16-row slowdown was seen. Both are recorded privately with the raw rows. The seam test `the_dynamic_step_says_which_depths_it_refuted_below_its_hit` pins the new field on the two-counter design of #1068's reproducer (either path may ship; `refuted_below` is the window's base, and the plain search's equals its depth).

## Not covered here

Which carrier the portfolio prefers when several finish (the fixed engine order, `sat-bmc` first) is unchanged, as decided on 2026-11-18. rIC3's own `--bmc-dyn-step` traces (`ric3-bmc`) have the same window property and report no `trace_shortest`.
