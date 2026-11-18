# Consumer briefing: owned IC3 counterexamples under input constraints concretise (2026-11)

> Source of truth: [`Ic3::lift_in`](../../crates/mununu-core/src/adapter/btor2/ic3.rs) — surface: CLI+API — every verb that runs the reach portfolio with the owned IC3 member (`btor2 verify`, `sv verify-auto`, `--owned-only`), on the CLI and the HTTP API

**Audience:** anyone who verifies designs with BTOR2 `constraint`s, such as an SVA `assume` or an input restriction, with the owned IC3 member. That member has been on by default under `--owned-only` and on hosts without rIC3 since #1053.

## TL;DR

- **A gap that cost real counterexamples is closed (mununu#1056).** The owned IC3 shrinks each counterexample state to the bits that force the next step. It took the design's constraints as given while doing so, so a shrunk state could keep values where the counterexample's inputs are forbidden: "dead states". The chain then failed to turn into a concrete trace, and IC3 abstained: `did not concretise at step S of D (a cube kept a state with no constraint-satisfying input …)`.
- **Now each shrink also proves the constraints**, so no shrunk state keeps a dead state.
- **Measured** on HWMCC'20 `shift_register_top_w16_d8_e0`, on the search path where it failed: before, IC3 hit the 4 096-retry cap and abstained; now it is `violated` at depth 16 in about 11 s, two runs out of two, matching rIC3 `-e ic3`.
- **What can change:** an IC3 `⊥` / abstention can become a definite `violated`, and a counterexample that ships is replayed on the concrete simulator first, as before. IC3 never ships a `holds` without its invariant check, so no `holds` can flip.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Designs with constraints that IC3 used to abstain on may now come back `violated`, with a replayed trace. | Re-baseline any expected-`unknown` rows for constrained designs. |
| **monono** | Same, for every verb that runs the reach portfolio. | Same. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the 2026-11-17 head-to-head of the owned IC3 against rIC3 (M2 68); diagnosed on #1056.
- **Test:** `a_lift_under_constraints_keeps_no_dead_state`. The design is a constraint the shrink could ignore, in two polarities. On the parent one polarity needed a dead-state retry; here neither does, and both traces replay.

## Not covered here

- #1072's backing-up around dead states stays as a second line of defence. With this fix the measured designs no longer need it.
- The full M2 68 + 14 edn monitors re-measurement on this build runs in the image after the current one; the scoreboard row follows it.
