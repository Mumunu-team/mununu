# Consumer briefing: `safety-shape-not-reducible` lists the forms the lane decides (2026-11)

> Source of truth: [`reach_rescue::accepted_safety_shapes`](../../crates/mununu-core/src/adapter/reach_rescue.rs) and [`reach_rescue::SHAPE_REMEDY`](../../crates/mununu-core/src/adapter/reach_rescue.rs) — surface: CLI+API — the `bottom_reason.detail` of kind `safety-shape-not-reducible`, the `bottom-reason` and `safety-rescue-declined` notes of `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** anyone who reads the text of a residual ⊥ on an AG-safety property the rescue lane declined.

## TL;DR

- **Message text only (mununu#1121, U2.6).** Four messages about the same decline said "reshape the property, or a new reducer is needed" with three different lists of what the lane accepts — one of them still calling `A |=> C` rejected after its reducer had shipped. A new reducer is not something a property can supply (`docs/policies/failure-paths.md` Rule 5). Every message now quotes **one list** of the forms the lane decides (`AG(atom)`, `AG(boolean of atoms)` over register-with-constant atoms, `A |=> C`, the bounded box chains with their limits) and **one remedy**: write the assertion in one of those forms; a bigger budget cannot help; an assertion already in one of them that still declines is a mununu limitation — please report it.
- **No verdict, kind, `determinism` or shape change.** The row stays `unknown` with kind `safety-shape-not-reducible`, `determinism: reproducible`; the note kinds `bottom-reason` and `safety-rescue-declined` keep their names; only `detail` / `summary` text reads differently. A consumer that matched the old sentence must update the match.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The `detail` of the kind and of two notes changes; the kind does not. | Nothing, unless you matched the old sentence (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | None (the UI shows `detail` verbatim). | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 row 6, mununu#1121).
- **Tests:** `x1121_the_shape_abstention_lists_the_forms_the_lane_decides_and_owns_the_rest` (the reproducer: the one-liner, the provenance note and the lane's decline all list `A |=> C`, own the remainder and never offer a reducer; compiles on the parent and fails there on the old sentence).

## Not covered here

- Which further forms the lane should decide (a register compared with another register, two box-bearing disjuncts) is a lever question for the wall-class matrix, not a wording one.
