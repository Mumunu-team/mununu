# Consumer briefing: a violation read only at a doubtful start cell is withheld (2026-11)

> Source of truth: [`BottomReason::ViolationAtUnestablishedInitialCubes`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) and [`kmts_lift::cycle_zero_feasible_cubes`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` rows decided by the predicate-cube engine, and the `warnings` of `mununu btor2 cegar` / `POST /api/v1/btor2/cegar`

**Audience:** anyone who gates on `sv verify-auto` verdicts, or reads `btor2 cegar` output, on designs where the cube path's cycle-0 filter runs (coupled predicates: two over one register, a compound, an atom over a combinational output).

## TL;DR

- **A `violated` can become `unknown` (mununu#1085).** The cube path's start set is filtered by a Z3 query per candidate cube, and the filter drops a cube on a proven `Unsat` only. A cube kept on doubt — an inconclusive query, a run budget that had already expired, a design or memory `init` the encoder could not take, or a candidate set that proved empty throughout — may be one the design never occupies at cycle 0. A `violated` read **only** at such cubes is now withheld as `unknown` with the new `bottom_reason.kind` **`violation-at-unestablished-initial-cubes`**; `detail` names each cube over the row's predicates and why it was kept.
- **`determinism` tells you what to do.** `host-dependent` means a clock or the `MUNUNU_CUBE_SMT_RLIMIT` resource bound caused the doubt, so a re-run with more of it may decide the row. `reproducible` means an encoding gap of the input; supply an `init` the filter can encode or let another engine decide the row.
- **No change** to a `violated` read at an established start cell, to `holds`, to rows decided by other members, or to the ⊥ re-plan (it still runs on the withheld row, so another engine may decide it).
- **`btor2 cegar`**: its verdict is a tally over every lifted cell with no start-state semantics, so it is unchanged; the same doubt now reaches its `warnings` list (`[mununu#1085] …`), saying which start cells the tally may be counting from without a cycle-0 state behind them. No shape change.
- **Where the #1076 witness check already applied**, the label was already `unknown`: the witness search's reach member proved the target unreachable and the row carried the `engine-contradiction` soundness alarm. The visible change there is the reason: from a false alarm to an honest withholding, with the cube and its remedy named.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A gate that counted a `violated` may now see `unknown` with kind `violation-at-unestablished-initial-cubes`. Treat it as the other withheld-VIOLATED kinds (`unestablished-initial-state`, `violation-over-free-leaves`): not a pass, not a confirmed bug. | Add the kind to the withheld set; branch on `determinism` for the retry decision. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: `bottom_reason` keeps its shape (`kind`, `detail`, `determinism`); one more `kind` value. | None, unless the UI enumerates kinds. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the change. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the review of mununu#1076's class (2026-11-18): the second member, split out as #1085. The one instance measured before it was mununu#1061 (a memory `init` the filter dropped), fixed by encoding the init; this change covers the class.
- **Tests:** `x1085_a_violation_read_only_at_a_doubt_kept_initial_cube_ships_unknown_not_violated` (the reproducer, compiles and fails on the parent), `x1085_the_withheld_violation_names_its_doubt_kept_cube`, `a_violation_read_only_at_doubt_kept_initial_cubes_is_withheld_and_named`, `the_cycle_zero_filter_names_the_cubes_it_keeps_on_doubt`.

## Not covered here

- An unnamed next-less state in a property atom's cone read per step by the exact engine: mununu#1086, the third member of the class.
- `btor2 verify --engine cube` and `btor2 verify-recoverability` read their own reset cube, not the filtered start set; nothing changes there.
