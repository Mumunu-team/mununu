# Consumer briefing: the exact engine's abstentions and input-atom refusals name a remedy the verb has (2026-11)

> Source of truth: [`symbolic_bitblast::OTHER_ENGINE_REMEDY`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) and [`symbolic_bitblast::input_atom_remedy`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI+API — the exact engine's own message as it reaches `bottom_reason.detail` (`engine-did-not-complete`), a `refused by exact: …` entry, an `abstained on the …` note and a `Skipped` reason on `sv verify-auto`, the `btor2 verify` verbs, `sv verify-*`, `sv mutate` with `--engine exact-symbolic`, and their API peers

**Audience:** anyone who reads the text of an exact-symbolic abstention (bit cap, node, iteration, fixpoint or wall-clock budget) or of a refusal of an atom over a primary input.

## TL;DR

- **Message text only (mununu#1121, U2.2 and U2.3).**
  - **U2.2.** Nine abstentions ended with "or use `--engine explicit`". That flag belongs to `sv verify-auto` and `sv mutate` (the cegar verbs take it too but refuse `--engine exact-symbolic`, so they never see this engine); the same engine runs under the `btor2 verify` verbs (whose flag is `--engine cube`), the liveness / recoverability / justice / FSM verbs and `sv verify-*` (no engine flag), where the SAT members run beside it on a safety row. Every abstention now closes with one clause: the knob, then the other engines **by the verbs that take them**, and what the abstention is to the row where a portfolio runs (one member's abstention, not the row's verdict).
  - **U2.3.** Two refusals of an atom over a primary input offered "the default `explicit` cube engine, or lift the antecedent through a shadow register". The shadow register was retired by inputs-in-the-state (mununu#677). The refusal now names the cause the run can change: with the lift opted out, drop `--no-input-state` (API `input_state: false`, `MUNUNU_NO_INPUT_STATE`); a controllable input on the game verbs, write the atom over the register it drives; anything else, a mununu defect to report.
- **No verdict, kind, `determinism` or shape change.** The row abstains or refuses exactly as before; the strings carried into `detail`, `refused by exact: …`, notes and `Skipped` reasons read differently. A consumer that matched the old closing sentence must update the match.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The engine's text inside `detail` / refusal lists changes; kinds and budgets do not. | Nothing, unless you matched "or use `--engine explicit`" or "shadow register" (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | None (the UI shows `detail` verbatim). | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 rows 2 and 3, mununu#1121).
- **Tests:** `x1121_an_exact_abstention_names_the_other_engines_by_the_verbs_that_take_them` and `x1121_the_input_atom_refusal_names_the_opt_out_that_caused_it` (the reproducers; both compile on the parent and fail there on the old sentences), `x1121_the_input_atom_remedy_follows_the_cause` (the seam: the three causes).

## Not covered here

- `symbolic_engine.rs`'s "use `--engine explicit`" for a derived predicate under `--engine symbolic`, and the two `sv verify-auto` notes that name `--engine explicit`: those verbs take the flag; unchanged.
