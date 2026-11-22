# Consumer briefing: the reach portfolio names a dead member and an unreplayed IC3 counterexample (2026-11)

> Source of truth: [`run_ic3`](../../crates/mununu-core/src/adapter/reach_portfolio.rs), [`settle_join`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) and [`collect_owned`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `btor2 verify`, `POST /api/v1/btor2/verify` (the replay list); the `refused` lines on every verb that runs the portfolio

**Audience:** anyone who reads `refused`, `abstained` or `trace_replay_failed` from the BTOR2 verify surfaces, or matches on their text.

## TL;DR

- **`trace_replay_failed` can be non-empty without `witness` on `btor2 verify` (mununu#1143).** The owned `ic3` member replays inside the engine; on a design with a cell wider than 128 bits its `violated` ships unconfirmed by the simulator, and the row `{member: "ic3", reason: "not replayed: …128…", sole_decider}` is now listed whether or not a witness was asked for. Before, the row appeared only with `witness`; without it the verdict read like a replayed one. A consumer that treats `sole_decider: true` as "unconfirmed" now sees it in that case too. The other verbs (`sv verify-auto`, the liveness, recoverability, fsm-scan and cdc-lint verbs) dropped the list on their way to the report until mununu#1175 — see [`2026-11-unconfirmed-paths-on-every-verb.md`](2026-11-unconfirmed-paths-on-every-verb.md).
- **One verdict corner changes, in the sound direction:** on a wide design whose own witness fails to parse for a reason other than the width, IC3 used to ship `violated` as "unavailable"; it abstains now (only the simulator's width refusal is "unavailable").
- **A new `refused` reason text:** "`<member>`: the member's thread ended without a result (it panicked) — a mununu defect, not something the input can change: please report it …" for `native`, `btormc`, `pono`, `ric3`, `ric3-bmc`, `interp`, `sat-bmc`, `ic3`, and the owned-only `cex` / `boolector` (the `exact` member already said this).
- **An owned-only run no longer calls a dead member "still running at the owned-only wall deadline"**: a member whose thread panics sends its death notice while the others run on, and is in `refused` with the line above; that `abstained` line is now only for a member whose thread was alive at the deadline.
- **No verdict changes. No field added or renamed**; the API schema's description text for `trace_replay_failed` is updated.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `trace_replay_failed` may appear on a `violated` without `witness`; one more `refused` text. | Read `sole_decider` as before; nothing else. |
| **monono** | Same. | Same. |
| **mununu-ui** | Same lists, shown through the generic path. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the lists. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the R6 review (mununu#1143).
- **Tests:** `x1143_the_owned_collector_tells_a_dead_member_from_one_still_running`, `x1143_a_panicked_member_join_is_a_named_refusal`, `x1143_an_unreplayed_ic3_violation_is_listed_without_a_witness`, `x1143_only_the_width_refusal_reads_as_unavailable`.

## Not covered here

- The justice rungs' joins are mununu#1140 (its own PR).
- A member that panics is still not retried; the run abstains for that member.
