# Consumer briefing — an `ltl` annotation row shows its formula as written; the mu-calculus moves beside it (2026-10)

> Source of truth: [`PropertyVerdictView`](../../crates/mununu-core/src/api/models.rs) (`formula`, `formula_language`, `translated`) + [`AutoVerifyReport::written_formula`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`render_verify_auto_text`](../../crates/mununu-cli/src/main.rs) — surface: (CLI+API) — `mununu sv verify-auto` (text and `--json`) and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono, mununu-ui, and anyone who reads or diffs the `formula` of a verify-auto row.

## TL;DR

- **An `@mununu_guarantee ltl …` row now reports the LTL its author wrote, not the mu-calculus it was translated into.** The text report prints `formula: ltl G (start_a -> X (!start_a W start_b))` and, on the next line, `translated: (nu G1. …)` — the mu-calculus the engine decided. The JSON row carries `formula` (the LTL, without the keyword), `formula_language: "ltl"` and `translated` (the mu-calculus).
- **Every other row is unchanged in value and gains one key.** SVA rows, plain mu-calculus annotation bodies and template instances keep `formula` as the mu-calculus and carry `formula_language: "mu-calculus"`; they have no `translated` key (absent, not `null`).
- **Why both texts.** The LTL is what the author meant and what a reviewer reads; the mu-calculus is what was decided, and it has to stay readable in the report — the mununu#821 weak-until defect was found by reading that translated spelling. Nothing is dropped; the two lines just trade places.
- **This reverses one line of the 2026-10 `ltl` bodies briefing**, which said the row's `formula` shows the translation, not the LTL. It now shows the LTL; the translation is `translated`.
- Verdicts, row names, labels, `--expect`, exit codes, traces, notes and every other field: unchanged. The schema `sv-verify-auto-response.schema.json` is regenerated for the two new keys.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | for an `ltl` annotation row, the stored `formula` string becomes the LTL as written (`G (…)`), and `translated` holds what `formula` held before; `formula_language` tells the two apart. Rows from SVA and mu-calculus bodies: same `formula`, plus `formula_language: "mu-calculus"` | if you diff or re-parse `formula` strings, read `translated` when it is present (`translated ?? formula` is always mu-calculus); if you only display them, nothing |
| **monono** | same as rosf; the text report of an `ltl` row has two lines where it had one | none required |
| **mununu-ui** | `SvVerifyAutoRunner` and `VerdictTable` render `p.formula`, so an `ltl` row now shows its LTL; the two new keys are optional on the client type | none required; optionally show `translated` under the formula |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the report rendering lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the verify-auto path consumers run |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `verify_auto::an_ltl_annotation_keeps_its_written_form_and_a_mu_calculus_body_does_not` (the scan: one `ltl` body, one mu-calculus body; the skeleton's table; the report's lookup) | pass — the `ltl` row's `written` is `Ltl` + `G (start_a -> X (!start_a W start_b))` while its `formula` still starts `(nu G1.`; the mu-calculus row's `written` is `None`; the table has one entry under `ann_guarantee_0` and the lookup answers `None` for `ann_guarantee_1` |
| `api::models::an_ltl_row_shows_its_written_form_and_the_translation_beside_it` (the wire: `formula` / `formula_language` / `translated` on both kinds of row; the key is absent, not `null`, on a mu-calculus row) | pass |
| `api_schema_drift_sv_verify_auto_response` (the committed schema matches the types) | pass after regeneration (`MUNUNU_UPDATE_API_SCHEMAS=1`); the other 24 drift tests unchanged and passing |
| `an_ltl_annotation_body_is_translated_over_every_path` and the rest of the annotation tests (the engine text is still the translation) | pass — 6 targeted tests, 0 failed; `cargo check --workspace --all-targets` and `cargo clippy -p mununu-core -p mununu-cli --features api,ast-extract --all-targets -- -D warnings` clean |
| `mununu sv verify-auto` on the tutorial's pump-pair draft (private material; host build `8f856d6-dirty`, the YosysHQ bundle's yosys 0.70 + yosys-slang, `--frontend slang`, 4 s) | the SVA row prints `formula: nu X. (((!((pump_a && pump_b)))) && [] X)` and nothing else, as before; the `alternates` row prints `formula: ltl G (start_a -> X (!start_a W start_b))` and `translated: (nu G1. (((!(start_a)) || ([] (nu W2. ((start_b || (!(start_a))) && (start_b || ([] W2)))))) && [] G1))`; `--json` gives the SVA row `formula_language: "mu-calculus"` with no `translated` key and the three `ltl` rows `formula_language: "ltl"` with `formula` = the LTL and `translated` = the mu-calculus; verdicts (HOLDS / VIOLATED / HOLDS / HOLDS) and exit code 2 unchanged from the 0.5.6 transcripts |

## Provenance

- Request: the formula line of an `ltl` annotation row should read as LTL; mu-calculus rows stay mu-calculus (2026-10-29).
- The row struct keeps the mu-calculus (`PropertyVerdict::formula` is re-parsed by the vacuity, witness and rescue passes), so the written form rides on `TranslatedAssertion::written` and on the report's `ModelDiagnostics::written_formulas` table, keyed by row name; both surfaces join it at render time through `AutoVerifyReport::written_formula`.
- The `ltl` bodies briefing: [`2026-10-ltl-annotation-bodies.md`](2026-10-ltl-annotation-bodies.md); the weak-until spelling that motivated keeping the translation readable: [`2026-10-ltl-weak-until-witness.md`](2026-10-ltl-weak-until-witness.md).

## Not covered here

- SVA rows still show their mu-calculus translation, not the SVA source; the assertion's own text is reachable through `label` and the source file, and an "as written" SVA would need the translator to keep the assertion text, which it does not today.
- CTXDSL `body = ltl …;` properties in `context eval` / `context verify`: a separate surface, not changed by this.
- The LTL is shown verbatim as written (trimmed), not re-printed from the parsed tree: the LTL AST has no printer, and the verbatim text is what the author recognises.
- `sv extract-sva --json` (the translation report) does not carry annotations and is unchanged.
