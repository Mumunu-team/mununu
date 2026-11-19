# Consumer briefing: a memory `init` that did not encode says whose gap it is (2026-11)

> Source of truth: [`kmts_lift::MemoryInitGap`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) and [`InitialCubeDoubt::remedy`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) — surface: CLI+API — the `violation-at-unestablished-initial-cubes` `bottom_reason.detail` of `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto`, and the `[mununu#1085]` warning of `mununu btor2 cegar` / `POST /api/v1/btor2/cegar`

**Audience:** anyone who reads the text of a withheld verdict or a cegar warning on a design with a memory `init` (the predicate-cube route's cycle-0 filter).

## TL;DR

- **Message text only (mununu#1106 item 6).** When a memory `init` does not encode for the cycle-0 query, the doubt the row carries (mununu#1085) used to end with one remedy for every cause: "supply a memory `init` the encoder takes, or decide the row with another engine". The cause is now classified and the remedy follows it:
  - **a construct the encoder has no rule for** (an operator in the `init` term): "a mununu limitation … the exact engine reads the memory itself when the cone fits its bit cap — decide the row with `--engine exact-symbolic`, or supply an `init` without `<op>`";
  - **a term the lift produced malformed** (a width mismatch, a constant outside its sort, a memory without an array sort, an array under a bit-vector-only theory): "a lift defect, not something the source can change: please report it with the BTOR2 (mununu#1106)".
  The doubt's description also names the class (`operator \`<op>\` is outside the encoder` / `the lift's \`init\` term is malformed`).
- **No verdict, kind, `determinism` or shape change.** The row is withheld exactly as before (`unknown`, kind `violation-at-unestablished-initial-cubes`, `determinism: reproducible`); only `detail` reads differently. A consumer that matched the old remedy sentence must update the match.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The `detail` text of a withheld row over a memory-init doubt changes; the kind does not. | Nothing, unless you matched the old sentence. |
| **monono** | Same. | Same. |
| **mununu-ui** | None. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (mununu#1106 item 6), after mununu#1085 surfaced the doubt itself.
- **Tests:** `x1106_a_malformed_memory_init_is_reported_as_a_lift_defect_not_as_the_users_gap` (the reproducer: a 2-bit `init` value on a 1-bit-element memory; compiles on the parent and fails there on the old sentence), `x1106_the_memory_init_gap_names_whose_gap_it_is_and_the_remedy_per_cause` (the seam: every encoder error class and its remedy).

## Not covered here

- `btor2 cegar` still ships a definite `violated` beside the warning (the laptop's open point from the #1105 review): the tally has no start-state semantics; untouched here.
