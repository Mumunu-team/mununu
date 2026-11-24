# Consumer briefing: a refusal names a flag only on verbs that have it (2026-11)

> Source of truth: [`merge_opt_in_remedy`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L1313) — surface: CLI+API — the text of the clock refusals, of the include-directory refusals and of the cut-net clock note, on every operation that lifts SystemVerilog

**Audience:** anyone who reads, shows or matches on the text of a SystemVerilog lift refusal.

## TL;DR

- **Message text only (mununu#1203). No verdict, exit code, field or JSON shape changes.**
- **The clock refusals no longer name `sv verify-liveness-all`.** The sentence that says where `--merge-clock-domains` is listed the verbs that take it, and one of them has not existed since 0.5 (`sv verify-liveness --response` took its place; the command line answers the old name with a usage error, exit 64). The list now reads `sv verify-auto` / `sv verify-liveness` / `sv verify-recoverability` / `sv check-fsm` / `sv lint` / `sv mutate`. The rest of the sentence is unchanged.
- **Four texts said "on the `sv` verbs" for a flag only some `sv` verbs take.** `--include-dir` and `--cutpoint` are on `sv verify-auto`, `sv verify-liveness`, `sv verify-recoverability`, `sv check-fsm`, `sv lint`, `sv cdc-lint` and `sv mutate`, and not on `sv cegar`, `sv discover` or `sv emit-btor2-per-module`. The texts now end "on the `sv` verbs that take it":

  | where | before | now |
  |---|---|---|
  | a relative include directory when the working directory cannot be read | `… give --include-dir an absolute path on the sv verbs` | `… on the sv verbs that take it` |
  | an include directory that is not a directory | `check the path given to --include-dir on the sv verbs` | `… on the sv verbs that take it` |
  | an include directory whose path slang cannot take as one token | `move or rename the directory given to --include-dir on the sv verbs` | `… on the sv verbs that take it` |
  | the note on a clock rooted at a cut net | `… or a cut point: --cutpoint on the sv verbs)` | `… on the sv verbs that take it)` |

- **API readers:** a text that reaches the HTTP API names the command-line flag there, as it did before. The request fields are `lift.cutpoints` and `lift.merge_clock_domains`; the lift request has no include-directory field (it carries the sources inline).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The five texts above. | None unless a lane matches on the old wording. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI affordance for the RTL verbs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Only to get the new texts. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use, or do not verify. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No, unless a lane needs the new texts: then bump the pin. | They consume the binary. |

## Provenance

- **Found by:** mununu#1203, reading the policy test that checks a flag named in a message against the verbs that can produce the message. A verb group in the sentence ("on the `sv` verbs") passed when any one verb of the group took the flag. Holding a group to every verb of it that reaches the message found the four texts; enumerating the forms a sentence gives a flag's scope found the retired verb, which the test dropped from the scope without a word.
- **Tests:** `a_flag_named_on_a_verb_group_is_on_every_verb_of_the_group_that_reaches` and `a_verb_the_cli_does_not_have_is_a_finding` (negative controls, in `tests/policy`), and `every_flag_a_message_names_is_on_the_verb_that_produced_it`, which now also fails on a verb of its own table that the CLI does not have.
- **Class:** `policy-scan-blind-spot` in [`docs/defect-classes.md`](../defect-classes.md), the rows on the scope of a flag.

## Not covered here

- A verb that can produce a message and is missing from the test's hand-kept table is still unchecked (mununu#1253), and a sentence that uses "on" for something other than the flag's scope would be misread (mununu#1254; no message has that shape).
- The earlier briefing on the clock refusals ([merged clock domains on every `sv` verb](2026-11-merged-clock-domains-on-every-sv-verb.md)) listed `sv verify-liveness-all` in a table of reports; the row is removed.
