# Consumer briefing: an owned-IC3 counterexample that does not concretise now abstains instead of aborting the run (2026-11)

> Source of truth: [`Ic3::concretize`](../../crates/mununu-core/src/adapter/btor2/ic3.rs) — surface: CLI+API — every verb that runs the reach portfolio with the owned IC3 member (`btor2 verify`, `sv verify-auto`, `--owned-only`), on the CLI and the HTTP API

**Audience:** anyone who runs the reach portfolio with the owned IC3 member, which has been on by default under `--owned-only` and on hosts without rIC3 since #1053.

## TL;DR

- **A crash is fixed (mununu#1079).** When the owned IC3 found a counterexample chain it could not turn into a concrete trace, it read a value from the SAT solver after an UNSAT answer, and CaDiCaL aborted the **whole process**: `cadical: fatal error: invalid API usage of 'int CaDiCaL::Solver::val(int)' … can only get value in satisfied state`. The run died with no report.
- **Now that member abstains.** The `ic3` member records `IC3 stopped …` with the reason (`did not concretise at step S of D …`), and the rest of the portfolio carries on.
- **Which runs were exposed.** Every version since #1072 hit it on any design whose chain hits the retry cap or has no initial state. Before #1072, only a chain with no initial state reached that read.
- **No definite verdict changes.** The path never shipped a verdict; it either crashed or now abstains.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A run that used to die on a CaDiCaL fatal error now returns a report; the `ic3` member may appear in `abstained` with the concretisation reason. | None, unless your harness treated that crash specially. |
| **monono** | Same, for every verb that runs the reach portfolio. | Same. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the falsifier run for #1056 on HWMCC'20 `shift_register_top_w16_d8_e0`. With main `3d1c235a` plus #1072's `ic3.rs`, the run aborted twice; with this fix it abstains twice.
- **Test:** `a_concretisation_that_stops_abstains_instead_of_reading_a_model`, a chain outside `Init` (dry step 0). It panics in the cadical crate on the parent.

## Not covered here

- #1056, which is still open. The #1072 backing-up does not decide its own row: on the `3d1c235a` trajectory it blocks 4 096 dead states (the cap) and step 10 still has no constraint-satisfying input. Main `6831311e` decides that row (`violated`, depth 16) only because N5.6 changed the search, so the concretisation gap is untouched.
