# Consumer briefing: messages print values in their own names, not as Rust `Debug` dumps (2026-11)

> Source of truth: [`predicate_expr::Token` `Display`](../../crates/mununu-core/src/adapter/btor2/predicate_expr.rs), [`LtlFormula` `Display`](../../crates/mununu-core/src/ltl/ast.rs), [`mu_calculus::Node::kind_name`](../../crates/mununu-core/src/mu_calculus/mod.rs) and the `Op::keyword` / `as_str` / `name` accessors the sites read — surface: CLI+API — the text of errors, refusals, `bottom_reason.detail`, notes, `Skipped` reasons and warnings across the adapters, the verify orchestrator, the composition checks and the API error bodies

**Audience:** anyone who matches on the text of an error or a note, or who reads one.

## TL;DR

- **Message text only (mununu#1121, U2.8).** About 130 messages printed an internal value with Rust's `{:?}`: a parse error as its struct (`ParseError { line: 1, … }` where the error's own sentence existed), a list of signal names as `["a", "b"]`, a BTOR2 operator as its enum variant (`Rol` for `rol`), a parser token as `Some(Ident("x"))`, a width as `Some(8)`, an LTL clause or a formula node as its syntax tree. Each now prints the value in the input's own words: the error's sentence, `a, b`, `rol`, `` `x` `` / "end of input", `8` / "unknown width", `G (req -> F grant)`, "a `mu` fixpoint". The accepted shapes in the two `bad`-monitor refusals are named in property terms (`REG op VALUE` atoms, `&&`, `||`, `!`, boxes, `e throughout b[->1]`).
- **No verdict, kind, field or JSON shape change.** Only the text inside existing string fields differs. A consumer that matched the Debug form of one of these values must update the match.
- **Held, for the founder (the fix would change a JSON value):** `POST /api/v1/context/counterstrategy`'s `inverted_formula` (a formula `Debug` dump; needs a mu-calculus unparser), the API `RoutingView.class` (`PropertyClass` Debug name on the wire), `source_format` on the extraction response, and the CLI `--json` `predicate_source` / `terminated_with` of `btor2 cegar`. Those keep their current values.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Error / note / `detail` text reads differently where a Debug dump was embedded; fields and kinds unchanged. | Nothing, unless you matched a Debug form (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | Error strings it shows verbatim read better; nothing to change. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 row 8, mununu#1121; the per-site table is on the issue).
- **Tests:** `x1121_a_predicate_expr_parse_error_names_the_token_not_its_debug_form` and `x1121_an_unknown_controllable_input_lists_the_primary_inputs_by_name` (the reproducers; both compile on the parent and fail there on the Debug text), plus one unit test per new `Display` / name accessor (tokens of the three parsers, Promela type names, the sidecar init policy, template domains, property classes, controllability words, formula node kinds, BTOR2 node keywords, LTL formulas, the rescue verdict, widths).

## Not covered here

- The messages that still name a Rust identifier, a roadmap step or a tier (U2.7) and those that name no owner for a gap (U2.10) are separate rows of mununu#1121.
