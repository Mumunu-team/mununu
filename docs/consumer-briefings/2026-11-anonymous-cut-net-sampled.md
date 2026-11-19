# Consumer briefing: a symbol-less cut net in an atom's cone is sampled once per step (2026-11)

> Source of truth: [`input_state::free_states`](../../crates/mununu-core/src/adapter/btor2/input_state.rs) and [`input_state::anonymous_free_states_in_cone`](../../crates/mununu-core/src/adapter/btor2/input_state.rs) — surface: CLI+API — every verb that runs the exact-symbolic engine (`sv verify-auto --engine exact-symbolic` and its exact rescues, `btor2 verify --engine exact`, `btor2 verify-recoverability`, `btor2 game`)

**Audience:** anyone who verifies designs with `--cutpoint` or `(* blackbox *)` outputs the lift leaves unnamed, or hand-written BTOR2 with a `state` that has no `next` and no symbol, on the exact-symbolic engine.

## TL;DR

- **A wrong `violated` (or `holds`) becomes the right verdict (mununu#1086).** A `state` with no `next`, no symbol and no alias is free at every step — an input in all but its keyword. mununu#1062 sampled the NAMED ones into the state (`next f = f$next`) when a property atom's cone reaches them; the unnamed ones were skipped, so the exact engine read one value of the cell in the atom and another in the transition, and the verdict over it was neither reading of the design. Such a cell is now sampled the same way, under the name the engine already gives it, `state_<nid>`.
- **Opted out, it is refused.** With `--no-input-state` / `MUNUNU_NO_INPUT_STATE=1` the cell cannot be sampled, so an atom reaching it is refused like one reaching a named cut (`refused-input-atom`), and the message names the cell and says what it is.
- **No change** to the cube engines (`btor2 cegar`, the verify-auto cube path free every next-less state already), to designs whose cut nets carry a name, or to a design that already names another cell `state_<nid>` (the anonymous cell stays as it was, said on the log).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | An exact-engine verdict on a design with an unnamed cut or black-boxed output in a property's cone may flip; a `--no-input-state` run may now refuse such an atom instead of deciding it. | Re-run gates that recorded exact-engine rows on cut designs. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: no shape change; the exact engine's prepare note lists `state_<nid>` among the sampled inputs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the review of mununu#1076's class (2026-11-18): the third member, split out as #1086; the named sibling was mununu#1062 (fixed 2026-11-18).
- **Tests:** `x1086_an_anonymous_cut_net_in_an_atoms_cone_does_not_decouple_the_atom_from_the_step` (the reproducer: VIOLATED on the parent, HOLDS now, control still VIOLATED), `x1086_with_inputs_in_the_state_off_an_atom_reaching_an_anonymous_cut_net_is_refused`, `x1086_an_anonymous_cut_net_in_an_atoms_cone_is_sampled_under_its_synthesized_name`.

## Not covered here

- A violation read only at a start cell the cube path kept on doubt: mununu#1085 (its own briefing, `2026-11-violation-at-unestablished-initial-cubes.md`).
- The cube path's older spelling of the same cell, `mununu_state_<nid>` (`parser::nextless_states_free`), is unchanged.
