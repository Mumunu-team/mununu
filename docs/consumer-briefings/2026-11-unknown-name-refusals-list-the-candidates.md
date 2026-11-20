# Consumer briefing: an "unknown X" refusal lists the candidates (2026-11)

> Source of truth: [`adapter::candidate_list`](../../crates/mununu-core/src/adapter/mod.rs) and [`ControllerMode::names`](../../crates/mununu-core/src/context/mod.rs) — surface: CLI+API — the `details` of the `400` bodies for an unknown formula, automaton or `controller_mode` on the context endpoints; the text of the CLI's unknown-formula / automaton / template refusals; the parsers' unknown-keyword, controllability-word and guard-key errors; the verify config's unknown source-id and assumption issues; the BTOR2 passes' unknown-register reasons

**Audience:** anyone who reads or matches on a refusal that names something the input does not declare.

## TL;DR

- **Message text only (mununu#1121, U2.9).** Thirty-eight refusals said "unknown X" and stopped, with the set of valid names in scope. Each now lists the candidates — the whole set when it is small, the nearest few and a count when it is large (`candidate_list`: exact match, then shared prefix, then edit distance).
- **API:** the `400` bodies for an unknown formula / automaton (`/api/v1/context/...` endpoints) carry the list in `details` (`"formulas: a, b"` / `"automata: …"`); `message` is unchanged. The `controller_mode` refusal's `details` is now generated from the mode enum and includes `gr1`, which the CLI accepted and the API's list omitted. Where `details` was `null` it is now a string.
- **CLI:** the same refusals end with "— expected one of: …" (the shape `context eval` already used for automata); `templates instantiate` names the available ids; `--template-arg` for a template that was not given names the ones that were.
- **Parsers and passes:** the BTOR2 reader names the sort kinds and the nearest keywords; the formula parser lists the controllability words and the guard keys; the abstraction passes' "unknown register" reasons name the register and the design's registers; the verify config issues list the declared source ids and assumptions (`ConfigIssue` gains a `declared` field on those three variants, serialized with the issue).
- **No verdict change, no shape change on a verdict response.** A consumer that matched a `details: null` or a bare "unknown template: X" must update the match.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `400` `details` on the context endpoints are populated; CLI refusal lines are longer. | Nothing, unless you matched `details == null` (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | The context endpoints' `400` `details` now carry the candidate list, which the editor can show as is. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 row 9, mununu#1121; the per-site table is on the issue).
- **Tests:** `x1121_an_unknown_controllability_word_lists_the_accepted_ones` (formula parser) and `x1121_an_unknown_source_id_in_composition_members_lists_the_declared_ones` (verify config) — the reproducers; both compile on the parent and fail there with no list — plus unit tests for `nearest_names` / `candidate_list`, `ControllerMode::names` (covers every mode, round-trips), the BTOR2 keyword table and the extraction language names.

## Not covered here

- `yosys/mod.rs`'s "unknown front end" cache entry turned out to be unreachable through the cache lookup (it already treats such an entry as a miss); the message now says so and asks for a report.
