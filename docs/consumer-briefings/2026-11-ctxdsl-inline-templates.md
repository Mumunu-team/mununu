# Consumer briefing — a property template instantiated inside a CTXDSL file: the `template` clause (2026-11)

> Source of truth: [`parse_template_clause`](../../crates/mununu-core/src/context_dsl/parser.rs) + [`collect_formulas`](../../crates/mununu-core/src/context_dsl/realize.rs) + [`realize_with_templates`](../../crates/mununu-core/src/context_dsl/realize.rs) — surface: (CLI+API+UI) — every path that realizes a CTXDSL document.

**Audience:** monono, mununu-ui and anyone who writes or generates CTXDSL files, or reads the `context eval` / `context synth` text output.

## TL;DR

- **New, opt-in syntax.** A formula may carry `template ID { KEY = value; … }` in place of `body = …;`:
  ```
  formula trip_exists { over protection; template reachable { TARGET = Lof; } }
  ```
  It is expanded when the file is realized, through the same catalog `context eval --template` and the API's `template_ref` use (built-ins, the deployment's `mununu.toml` packs and, on the CLI, `--templates-file`), and the formula keeps the name the file gives it. The CTXDSL analogue of the RTL's `@mununu_template` annotation.
- **Refusals, never silent:** `body` and `template` together (a parse error naming the formula); an unknown template id, a missing argument or a value that fails the parameter's validation (a realization error naming the formula, with the registry's message). An identifier-typed parameter (`Predicate`, `State`) takes a bare identifier, as on every other surface.
- **CLI text output gains one line per clause**, before the verdict: `Template 'reachable' → formula 'trip_exists': mu X. (Lof || <> X)` — the line `--template` has always printed, on stderr.
- **No JSON shape change.** `context eval --json`, `/api/v1/context/eval` and `/context/synth` responses are unchanged; a clause-defined formula is reported exactly like a written-out one. `RealizedFormula` (Rust API) gains `template: Option<TemplateExpansion>`.
- **`template` is not a keyword.** A label, state or automaton named `template` keeps parsing; the word is recognised only where a formula's `body` would be.
- No existing verdict changes: files without the clause realize as before.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (RTL only) | none |
| **monono** | a CTXDSL file you generate or lint may carry `template` clauses; a tool that pattern-matches `body =` to find a formula's text no longer sees one for such a formula — the expanded text is the realized formula's `raw` | optional: emit the clause instead of expanding templates yourself; if you parse CTXDSL text, accept the clause |
| **mununu-ui** | the Monaco CTXDSL language should tokenize `template` and the `{ KEY = value; }` block; the template picker can insert the clause into the file instead of only sending `template_ref` | mununu-ui PR `feat/ctxdsl-inline-template-formulas` |
| **template packs** (`--templates-file`, `mununu.toml`) | a clause is instantiated against the same layered catalog; an id a pack defines resolves on the CLI with `--templates-file` and everywhere through the deployment config | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and want the clause | new DSL syntax |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | RTL image; the CTXDSL path inside it moves with the binary volume |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not realize CTXDSL |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | opt-in syntax |

## Measured

| run | result |
|---|---|
| `context_dsl::tests::a_formula_can_be_a_template_clause_in_place_of_a_body` | identifier, string and integer values parse; the trailing `;` after the block is optional |
| `context_dsl::tests::a_formula_with_both_a_body_and_a_template_is_refused` | parse error naming the formula |
| `context_dsl::tests::template_is_still_an_ordinary_identifier_outside_a_formula` | a label, a state and an automaton named `template` parse |
| `realize::tests::a_template_clause_expands_to_the_catalog_formula_and_keeps_the_declared_name` | `reachable { TARGET = Scram; }` → `mu X. (Scram || <> X)`, the same `raw` and class as the written-out formula, under the declared name |
| `realize::tests::a_template_clause_with_an_unknown_id_is_refused_naming_the_formula` · `…missing_a_required_argument…` · `…with_an_expression_where_an_identifier_is_due…` | realization errors naming the formula |
| `realize::tests::a_template_clause_resolves_from_the_callers_registry_too` | a pack template resolves through `realize_with_templates`; the configured catalog alone refuses the id |
| CLI, `context eval` on a three-formula file (built-in `reachable`, `mutual_exclusion`, a pack's `stays` with `--templates-file`) | the expansion line per clause; verdicts identical to the written-out formulas |

## Provenance

- Branch `feat/ctxdsl-inline-template-formulas`; PR pending.
- Docs: [`wiki/CTXDSL-Language-Reference.md`](../../wiki/CTXDSL-Language-Reference.md) §"A formula from the template catalog", [`wiki/Property-Templates.md`](../../wiki/Property-Templates.md) §"In the CTXDSL file", [`docs/ctxdsl-modelling-guide.md`](../ctxdsl-modelling-guide.md) §9.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The HTTP catalog (`/api/v1/templates`) and `template_ref` still build the registry without the server's `--config` packs; the clause follows the same rule over HTTP (built-ins + the deployment config the server process sees).
- `verify.toml` behaviour and the RTL annotation form are unchanged.
