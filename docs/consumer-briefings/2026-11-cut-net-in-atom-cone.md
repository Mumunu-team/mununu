# Consumer briefing: a cut net a property atom reads is sampled with the inputs (2026-11)

> Source of truth: [`free_states`](../../crates/mununu-core/src/adapter/btor2/input_state.rs) and [`cone_inputs`](../../crates/mununu-core/src/adapter/btor2/parser.rs) — surface: CLI+API — `--cutpoint` / `cutpoints` on `mununu sv verify-auto`, `sv verify-liveness`, `sv verify-recoverability` and their `POST /api/v1/sv/…` peers, when the exact engine decides the row

**Audience:** anyone who uses `--cutpoint` (or black-boxes a submodule) and reads verdicts the exact engine decides.

## TL;DR

- **A false `violated` is removed (mununu#1062).** When a property atom read a signal driven by a cut net, the exact engine evaluated the atom with one value of the cut net and the next-state function with another, in the same cycle. A property true for every value of the cut net could read `violated`, with no witness.
- **The cut net is now sampled like a primary input** (the inputs-in-the-state rule): one value per cycle, free at every cycle. The row is decided on the cut model as the other engines decide it.
- **Notes:** the `inputs in the state` note now lists the cut net among the sampled names. The cone is wider by twice its width.
- **No change** when no atom reads a cut net, and none without `--cutpoint` or a black box.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A `violated` (decided by `exact-symbolic`, `witness-unavailable`) under `--cutpoint` may become `holds`. `decided_by` and timings can change on such rows. | Re-run any gate that recorded such a row. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: no shape change. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** an internal reproduction run, 2026-11-17 (#1062): the exact engine and three reach engines gave opposite definite verdicts on one cut model.
- **Tests:** `x1062_a_cut_net_in_an_atoms_cone_does_not_decouple_the_atom_from_the_step` (the engine) and `x1062_a_cut_net_in_an_atoms_cone_is_sampled_like_an_input` (the rewrite); the issue's two reproducers in the `mununu-sva` image.

## Not covered here

- An unnamed next-less state in an atom's cone is still neither sampled nor refused: mununu#1076 tracks the class.
- On the two-player verbs a cut net is treated as an environment choice, like an uncontrollable input.
