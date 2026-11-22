# Consumer briefing: a path the simulator did not confirm is named on every verb (2026-11)

> Source of truth: [`ReachOutcome::unconfirmed`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L185) and [`member_notes`](../../crates/mununu-core/src/adapter/liveness_rescue.rs#L579) — surface: CLI+API — `sv verify-auto`, `btor2 verify-liveness` / `sv verify-liveness`, `btor2 verify-justice`, `btor2 verify-recoverability` / `sv verify-recoverability`, `btor2 check-fsm` / `sv check-fsm`, `sv cdc-lint`, `btor2 verify --fault … --target`, and their `POST /api/v1/…` peers

**Audience:** anyone who reads a `violated` (or a reached target, or a reached encoding) from a verb other than `btor2 verify`, or who matches on the text of `notes`, of a `cdc-lint` detail, or of a `trace_replay_failed` reason.

## TL;DR

- **What it is.** A reach-portfolio member can find a path to `bad` that the concrete simulator did not confirm: the owned `ic3` member on a design with a cell wider than 128 bits (the simulator cannot carry it), or a subprocess member whose witness did not replay. The verdict ships either way. `btor2 verify` has listed such a member in `trace_replay_failed` since mununu#1143; **every other verb dropped the list**, so its result read exactly like a replayed one (mununu#1175).
- **No verdict changes.** Only what is said beside a verdict.
- **`sv verify-auto`:** a row decided by the reach portfolio has `portfolio.trace_replay_failed: [{member, reason, sole_decider}]` — the same rows as `btor2 verify`. The text report prints `unconfirmed path: …` under the row. The antecedent-vacuity note (`antecedent-reachable`) and the `AG EF` trap-certificate note list the same lines among their `items`.
- **Response `verify-liveness` (one pair, under fairness, a conjunction; `btor2` and `sv`):** `notes` used to be empty on a decided row. A `violated` row whose lasso was not confirmed now has one line per member in `notes`.
- **Justice `verify-liveness` / `verify-justice`:** the l2s rung's unconfirmed lasso is a line of the verdict's `notes` (prefixed `l2s `), on the serial and the parallel ladder. Under `--parallel-rungs` the notes of every rung that answered are kept, so a row withheld because two rungs contradict each other shows which side rests on an unconfirmed lasso.
- **An undecided response or justice row** whose k-liveness fallback found a bound reachable on an unconfirmed path says so in the same abstention note that lists `<bound> reachable` (`at <bound>, <member>: …`).
- **`verify-recoverability` assumption discovery** (and `btor2 game`, which reports the same `holds_under` entries): `holds_under[].notes` (new, absent when empty) carries the lines of the non-vacuity gate's two reach checks.
- **`check-fsm`:** a register row has `notes` (new, absent when empty) when the illegal encoding was reached on an unconfirmed path; with `--liveness`, the inventory row has `notes` (`encoding <v>: …`).
- **`sv cdc-lint`:** the gray-step finding's `detail` text gains ` — <member>: the path it found was not confirmed …` after the unchanged first sentence.
- **`btor2 verify --fault … --target`:** the summary has `trace_replay_failed`, as plain `btor2 verify` does (CLI: always present, usually `[]`; API: absent when empty).
- **One line everywhere:** `<member>: the path it found was not confirmed on the concrete simulator (<reason>); no other member found a path, so this result rests on it alone` — or `…; <members> found a path too`. When another member proved there is no path (the row is then `unknown`: a contradiction), the line ends `…, and <members> proved there is none` and does not say a result rests on the path.
- **The `ic3` reason text changed** (also on `btor2 verify`): `not replayed: the concrete simulator carries cells up to 128 bits and this design has a wider one (…) — not supported by mununu yet (a mununu limitation; nothing to change in the design)`. Before: `…carries 128-bit cells; this design has a wider one (…)`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | New optional fields (`portfolio.trace_replay_failed`, `notes` on `check-fsm` rows and on `holds_under` entries); `notes` can be non-empty on a decided response row; the `cdc-lint` detail and the `ic3` reason text. | Treat `sole_decider: true`, or a `rests on it alone` line, as "this result has no replayed path". Do not match the old `ic3` reason text or the whole `cdc-lint` detail. |
| **monono** | Same. A formal lane that treats a non-empty `notes` on a response row as "undecided" must read `outcome` instead. | Same. |
| **mununu-ui** | The new fields arrive through the generic row; nothing is rendered specially. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the lists. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the review of mununu#1143, which fixed `btor2 verify` and filed the other verbs as mununu#1175.
- **Class:** `member-stop-reason` in [`docs/defect-classes.md`](../defect-classes.md), which holds the table of reports.
- **Tests:** `x1175_a_path_the_simulator_did_not_confirm_reaches_the_verify_auto_row` (the reproducer), `x1175_an_unconfirmed_path_says_whether_the_result_rests_on_it_alone`, `x1175_a_decided_response_row_names_a_member_whose_lasso_was_not_confirmed`, `x1175_the_l2s_rung_names_a_member_whose_lasso_was_not_confirmed`, `x1175_a_gray_step_found_on_an_unconfirmed_path_says_so`, `x1175_a_trap_certificate_lists_a_reach_path_that_was_not_confirmed`, `x1175_a_later_rungs_unconfirmed_lasso_note_survives_the_parallel_merge`, `x1175_a_bound_reached_on_an_unconfirmed_path_says_so_at_its_width`, and the enumerating `policy::unconfirmed_paths_reach_every_report_1175`.
- **API schemas:** `docs/api-schemas/` regenerated — the new optional fields in the `sv verify-auto`, `check-fsm`, `verify-recoverability` and `btor2 game` responses, and the `notes` description of the three `verify-liveness` responses.
- **Observed on the built binary, in the image:** `btor2 verify --owned-only` on a design with one 130-bit register lists `ic3` with the new reason text and `sole_decider: false` (three other members found the path). The other verbs' rows are pinned at their seams; on the small designs tried, another member decided before `ic3` and nothing was unconfirmed.

## Not covered here

- The simulator still carries cells up to 128 bits; a wider design's `ic3` path stays unconfirmed. This change makes that visible, it does not widen the simulator.
- `btor2 verify` itself: its list is unchanged in shape (mununu#1143); only the `ic3` reason text moved.
- The recoverability vacuity probe and the trap certificate's closure step report nothing new: only a proof yields a hint there, and a reachable `bad` in the closure step only rejects a candidate.
