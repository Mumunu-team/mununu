# Consumer briefing: a reach-portfolio member stopped by the clock keeps its progress (2026-11)

> Source of truth: [`clock_stopped`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `btor2 verify` and every verb that runs the full reach portfolio, on the CLI and the HTTP API

**Audience:** anyone who reads the reach portfolio's `abstained` list, in particular its reason text.

## TL;DR

- **A member that the portfolio's wall or run budget stops now says how far it got (mununu#1043).**
  - Until now, every owned member (native BMC / k-induction, SAT BMC, IC3, interpolation) treated any stop on the shared flag as "a peer decided". It then reported nothing.
  - So a member stopped by the wall lost its own progress, and the #1039 sweep wrote the generic `still running at the wall deadline; no result was collected`. A member stopped by the run budget, outside the wall, reported nothing at all.
- **The new `abstained` reasons** read `<progress>; stopped by the portfolio's wall or run budget before a verdict`, where `<progress>` is one of:
  - `native BMC / k-induction stopped at depth <k>`
  - `SAT BMC stopped; every depth below <n> refuted`
  - `IC3 stopped at frame <f>`
  - the interpolation member's own undecided reason
- **The #1039 sweep's text depends on the cause:**
  - wall: `stopped at the portfolio's wall deadline (<ms> ms, --timeout-ms) before it reached a verdict`
  - run budget: `stopped when the run budget expired, before it reached a verdict`
- **A member stopped because another member decided stays in no list**, as before. An external cancel is treated the same way.
- **No verdict changes value, and the JSON shape is unchanged.** Only the entries and the reason text in `abstained` change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `abstained` entries for members stopped by the wall or the budget carry the member's progress. | If your harness matches `abstained` reason text, accept the new text. |
| **monono** | Same, for every verb that runs the reach portfolio. | Same. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the new reasons. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the #1042 review (reading; structural).
- **Tests:**
  - `a_member_stopped_by_the_clock_keeps_its_progress_and_a_peer_stop_is_in_no_list`: the settle seam, with the clock flag on and off.
  - The `run_native`, `run_sat_bmc` and `run_ic3` cancel tests, which now assert the progress the member returns.
  - The interpolation member's test, for both causes.

## Not covered here

- The `--owned-only` driver has no wall watchdog. A stop there comes only from a peer's verdict, so it stays in no list.
- The subprocess members (btormc, pono, rIC3) carry no progress. When the wall or the budget stops one, it is now recorded in `abstained` with its existing `returned no verdict within its <ms> ms subprocess budget …` reason. Before, it was in no list.
