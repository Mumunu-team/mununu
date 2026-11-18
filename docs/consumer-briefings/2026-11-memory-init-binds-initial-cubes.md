# Consumer briefing: a memory's `init` line binds the cube's initial states (2026-11)

> Source of truth: [`smt_cube_feasible_at_cycle_zero`](../../crates/mununu-core/src/adapter/btor2/smt_must_edge.rs) and [`cycle_zero_feasible_cubes`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` and `mununu btor2 cegar` on a design with an array-sorted state cell

**Audience:** anyone who reads `sv verify-auto` verdicts on designs with memories or unpacked arrays.

## TL;DR

- **A false `violated` is removed (mununu#1061).** On a design with an initialised memory, the explicit cube could report a safety property `violated`, with no witness, at a start state that needed a memory value the design never has at cycle 0.
- **Such a row is now decided by another member or left `unknown`.** On the reproducer it is `holds`, decided by the reach rescue.
- **No change** for a design with no array-sorted state, and none for a memory without an `init` line (still free at cycle 0).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A `violated` with `witness-unavailable` on a design with an initialised memory may become `holds` or `unknown`. | Re-run any gate that recorded such a row. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: no shape change. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** an internal reproduction run, 2026-11-17 (#1061): two members gave opposite definite verdicts on one model.
- **Tests:** `x1061_a_memory_init_line_binds_the_initial_cubes`; the issue's four runs in the `mununu-sva` image.

## Not covered here

- A `violated` that the witness search itself contradicts still ships as `violated` on other paths: mununu#1076.
