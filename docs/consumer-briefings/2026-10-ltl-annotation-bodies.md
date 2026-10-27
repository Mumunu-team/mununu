# Consumer briefing — `@mununu_guarantee ltl …`: LTL bodies in SystemVerilog annotations (2026-10)

> Source of truth: [`scan_annotation_properties`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`translate_universal_text`](../../crates/mununu-core/src/ltl/universal.rs) — surface: (CLI+API) — every `--source` of `sv verify-auto` and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono and anyone who writes `@mununu_guarantee` annotations in SystemVerilog or pins their rows.

## TL;DR

- **Additive, no new flag.** A guarantee body may now start with `ltl`: `// @mununu_guarantee(name) ltl G (req -> F ack)`. It is read over EVERY path (the reading of a CTXDSL `body = ltl …;`) and translated to the mu-calculus text the row carries, so the row's `formula` field shows that text, not the LTL. A body without the keyword is mu-calculus exactly as before.
- **Comparison atoms in LTL.** The LTL parser accepts `ident op decimal` and `ident op ident [+ k]` (`state_q == 5`, `cnt_q == cnt_q__past + 1`) wherever LTL is parsed — in annotations and in CTXDSL `ltl` / `eltl` bodies, where a predicate name was the only atom before. A body that used `->` after an identifier is unaffected (`-` is not a comparison operator).
- **Refusals name their reason.** A formula outside the every-path fragment (`F G p`, `G p || G q`, `F (p && X q)`) and an `eltl` body are skipped under the `annotation-properties` note (scope caveat), never decided as a different property; `eltl`'s message spells out the mu-calculus form of the some-path reading.
- JSON shape, schemas, flags, exit codes, row names and labels: unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | annotation files may carry `ltl` bodies; a row's `formula` is the translated text (starts with `(nu G1.` for a `G` body) | none required; if you diff `formula` strings, expect the translation, not the LTL |
| **monono** | same; `--expect name=holds` on a named `ltl` annotation works as for any annotation | none required |
| **mununu-ui** | nothing (RTL verbs); CTXDSL `ltl` bodies gain comparison atoms, which the editor already accepts as text | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the scanner and the parser live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the verify-auto path consumers run |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `ltl::parser` — `comparison_atoms_parse_as_single_predicates`, `implication_is_not_a_comparison_and_a_dangling_operator_is_an_error` | MEASURED_PARSER |
| `verify_auto` — `an_ltl_annotation_body_is_translated_over_every_path` (response → `nu G1 … mu F2 … <> true`, comparison atom kept as `(state_q == 5)`, the text re-parses) | MEASURED_SCANNER |
| `verify_auto` — `an_ltl_body_outside_the_fragment_and_an_eltl_body_are_skipped_with_reasons` (`F G p`, `eltl`, empty `ltl`, and `ltl_ok` as a plain atom) | MEASURED_REFUSALS |
| the existing annotation, CTXDSL `ltl` / `eltl` and LTL-parser tests | MEASURED_EXISTING |

## Provenance

PROVENANCE_PLACEHOLDER

## Not covered here

- `@mununu_assume ltl …` (an assume stays `GF <atom>` or an untyped string); fairness-relativised `ltl` bodies (`translate_universal_fair`) are CTXDSL-only — a response guarantee under fairness keeps the `@mununu_assume GF <atom>` route.
- `eltl` in annotations: deliberately not accepted (the plain body is that reading).
- A printer from a mu-calculus `Formula` back to text: the universal translation produces text directly, so none was added.
