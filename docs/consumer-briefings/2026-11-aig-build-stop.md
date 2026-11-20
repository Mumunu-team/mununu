# Consumer briefing: the owned SAT members' model build stops, and is built once (2026-11)

> Source of truth: [`AigModel::build_with`](../../crates/mununu-core/src/adapter/btor2/aig.rs) and `SharedAig` in [`adapter/reach_portfolio.rs`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — every verb that runs the reach portfolio (`btor2 verify`, `sv verify-auto`, the liveness and recoverability verbs, their API peers)

**Audience:** anyone who reads `abstained` from the portfolio verbs, pins a container's memory, or runs the owned members on large designs.

## TL;DR

- **A new `abstained` reason family (mununu#1142)** for `sat-bmc` and `ic3`: `aig: the model passed the node cap while being built (<n> nodes, cap <c>); MUNUNU_AIG_NODE_CAP raises the cap (at most 2147483647)` and `aig: the deadline expired while the model was being built (<n> nodes so far)`. Before, the bit-level model was built with no stop: a member's wall, a peer's verdict and the run budget applied only to the solve after it, and memory was bounded by the host alone.
- **A new variable:** `MUNUNU_AIG_NODE_CAP` (a node count; default 67,108,864). A design that trips the default cap abstains on both owned bit-level members with the count; the other members are unaffected.
- **One model per run:** the two members share one build; a design that used to be bit-blasted twice is bit-blasted once. No verdict change.
- **No field or JSON shape change.**

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | One more `abstained` text family; large designs build once. | Nothing, unless a gate matches `abstained` text. |
| **monono** | Same. | Same. |
| **mununu-ui** | Same lists, shown through the generic path. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the stop. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the R6 review (mununu#1142).
- **Tests:** `x1142_the_build_stops_at_the_node_cap_and_names_the_count`, `x1142_a_cancelled_or_expired_build_stops_at_once`, `the_node_cap_variable_is_a_clamped_count`, `x1142_the_sat_members_share_one_aig_model_and_its_stop`.

## Not covered here

- The cost of the build on a wide multiply or divide cone is not measured here; the cap is a ceiling, not a tuned value.
