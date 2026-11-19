# Consumer briefing: what a composition decided now travels with the verdict (2026-11)

> Source of truth: [`ContextVerifyResponse::notes`](../../crates/mununu-core/src/api/models.rs), [`RealizedContext::notes`](../../crates/mununu-core/src/context_dsl/realize.rs) and [`composition::compose_with_notes`](../../crates/mununu-core/src/composition/mod.rs) — surface: CLI+API+UI — `mununu context eval` (text `Notes` block, `--json` key `notes`), `POST /api/v1/context/eval` and the MCP `context_eval` tool (`notes`, and `note:` lines in the text summary), the UI client type `ContextVerifyResponse`

**Audience:** anyone who reads `context eval` / `/context/eval` results over a CTXDSL model with a `composition { … }` block — in particular a realizability gate over a composed system.

## TL;DR

- **New field, additive (mununu#1106 item 5).** A decision taken while realizing the model that changes what the model means now reaches the result as a string in `notes`, each prefixed by the composition's name and naming the members the user wrote. Two kinds today:
  - `[mununu#592]` **controllability demotion**: a label controllable in one member and carried by its partner composes as **uncontrollable**, so a controller that could withhold it in the member alone cannot in the product, and a realizability verdict over the composition can differ from one over the member. Until now this was a `tracing::warn!` line worded "the left member" — invisible on the API and unnamed on the CLI.
  - `[mununu#570]` **dead compound action** let through by `allow_dead_vectors;`: a multi-label transition no partner matches can never fire, so its automaton freezes where it would take it.
- **No verdict changes.** The product and every verdict over it are the same; what changes is that the reading context is on the result. `notes` is absent when there is nothing to say (`skip_serializing_if = Vec::is_empty`), so a client that does not look for it sees the old shape.
- **CLI:** `context eval` prints a `Notes (realization decisions that shape this verdict):` block before the verdict; with `--sanity` and no formula, `note:` lines before the exit; `--json` carries the same strings under `notes` (present only when non-empty, as on the API).
- **Schema:** `docs/api-schemas/context-eval-response.schema.json` regenerated with the new optional array.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A realizability gate over a composed CTXDSL model gets `notes` when a controllable label is shared with a partner. Treat a `[mununu#592]` note as "the verdict is over a product in which this label is uncontrollable" — a `holds`/`realizable` beside it is a verdict on that reading. | Surface `notes` next to the verdict; nothing to parse (free text, stable prefix `composition \`<name>\`: [mununu#<n>]`). |
| **monono** | Same. | Same. |
| **mununu-ui** | `ContextVerifyResponse` gains `notes?: string[]` (companion client change in the mununu-ui repository). | Render the list beside the verdict when present. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the field. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (mununu#1106 item 5): the demotion that can flip a realizability verdict reached only the log, and said "the left member".
- **Tests:** `x1106_a_controllability_demotion_reaches_the_verify_response_as_a_note` (the reproducer: the API response carries the note; compiles and fails on the parent), `x1106_compose_with_notes_returns_the_demotion_with_its_side_and_names_the_members`, `x592_a_shared_controllable_label_composes_as_uncontrollable` (now also asserts the note), `x1106_a_dead_compound_action_let_through_is_a_note_and_a_clean_composition_has_none`.

## Not covered here

- `mununu verify` over N sources (the orchestrator) composes nothing through this path; adapter warnings it drops are mununu#1106 item 3.
- The yosys multi-module fold uses `compose` (log only): an RTL lift has no controllable labels and no compound actions, so no note can arise there.
