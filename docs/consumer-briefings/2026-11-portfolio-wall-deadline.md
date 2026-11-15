# Consumer briefing: the reach portfolio returns at its wall deadline (2026-11)

> Source of truth: [`stopped_by_wall_unrecorded`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `btor2 verify` (with or without `--timeout-ms`), every verb that runs the full reach portfolio, on the CLI and the HTTP API; `--bmc-only` native (CLI-only)

**Audience:** anyone who runs `btor2 verify` under an outer time limit, or who reads the reach portfolio's `abstained` list.

## TL;DR

- **The full reach portfolio now stops at its own wall (mununu#1039).** That wall is the members' budget (`--timeout-ms`, default 60 000 ms) plus a 250 ms grace.
  - Until now, nothing stopped the native BMC / k-induction member at that wall when no run budget was set. Its per-check limit is a deterministic z3 resource count, not a clock.
  - On a loaded host, one check could run past the wall: measured on the box, 70 s late on HWMCC'20 `mul9`. On `shift_register_top_w16_d64_e0` it ran past an outer `timeout 420`, which killed the run before any JSON was written.
- **A member the wall stops is named in `abstained`**, with the cause `stopped at the portfolio's wall deadline (<ms> ms, --timeout-ms) before it reached a verdict`. This is a new entry in an existing list; the shape is unchanged.
- **`--bmc-only` with the native engine now honours `--timeout-ms`.** A stopped search sets `bmc_stopped` to `stopped: <cause>`. Without `--timeout-ms` the search is unbounded, as before. Measured here: `elevator.4` ran 2 h 51 min under a 300 000 ms `--timeout-ms` before this change.
- **No verdict changes value.** A member the wall stops abstains; it never contributes a verdict. A run that used to be killed now returns `unknown` with its causes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `btor2 verify` returns within `--timeout-ms` + 0.25 s plus shutdown. `abstained` may carry the new wall-deadline entry. | If your harness keys on `abstained` reasons, accept the new reason text. |
| **monono** | Same, for every verb that runs the reach portfolio. | Same. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the wall. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the N5.4 rerun and the C6 attribution run on the box (HWMCC'20 M2 rows, `--timeout-ms 300000` under `timeout 420`), and the #1037 end-to-end check on a laptop (`--bmc-only`).
- **Tests:**
  - `the_portfolio_returns_at_its_wall_and_names_the_member_it_stopped`: a member that sleeps 20 s, under a 200 ms wall. With the wall check disabled, the test fails at 20.0 s.
  - `the_wall_sweep_names_only_silent_started_members`: the post-join sweep.

## Not covered here

- The native member's per-check z3 resource limit (#893) still converts milliseconds to resource units at a fixed rate. On a slower host one check can take longer than nominal; the wall now bounds the run, but not that check's own cost.
- native k-induction's fixed depth cap of 40 (#1038).
