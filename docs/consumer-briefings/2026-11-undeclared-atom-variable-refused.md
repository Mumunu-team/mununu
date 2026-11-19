# Consumer briefing: an atom over an undeclared variable is refused, not true everywhere (2026-11)

> Source of truth: [`EvaluationError::UndeclaredVariable`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) and [`evaluate_expression_on_demand`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) — surface: CLI+API+UI — `mununu context eval`, `POST /api/v1/context/eval` / the MCP `context_eval` tool, and every verb that evaluates a μ-calculus formula over a CTXDSL model with `variables`

**Audience:** anyone who evaluates formulas over a CTXDSL model that declares `variables` (unrolled numeric state), by CLI, API or the UI.

## TL;DR

- **A verdict can become an error (mununu#1106 item 7).** A variable atom (`n == 2`, `p < q`, `flag == true`) that reads a name **no state of the model carries a value for** used to resolve to `Maybe` and be included — it held at **every** state (`docs/ctxdsl-modelling-guide.md` §3 documented it). A misspelled name made a safety property vacuous and a reachability property spuriously true; the same typo in a guard was already a hard error. The atom is now **refused**: the error names the atom, the unknown variable(s), the variables that do carry per-state values, and the remedy (check the spelling; declare the variable with a numeric type; for a `bool` variable, which publishes no per-state value, write a `predicate` over it).
- **A bare unknown name** (`nosuchpred`, with no comparison) is no variable expression: it now falls to the documented under-approximation for an unknown predicate (assumed false), the same as on a model without `variables`, instead of `Maybe → true`.
- **No other verdict changes.** An atom over a declared numeric variable binds as before; a variable some states carry and others do not keeps the per-state `Maybe → include` reading. Measured on the in-repo corpus (every integration and CLI test, 109 CTXDSL models in `examples/` and the wiki): no model carried such an atom, so no pinned verdict moved.
- **Wire shape:** unchanged. On the API the error is the ordinary evaluation error body (`success: false`, the message above); on the CLI the command exits with it.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A formula with a misspelled variable, or one over a `bool` variable, now fails to evaluate instead of holding. Any gate that counted such a `holds` was counting a vacuous verdict. | Fix the spelling or declare the variable numerically / as a predicate; treat the new error as a model defect, not a tool failure. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: the error reaches the UI through the existing error path of `/context/eval`. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the refusal. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (mununu#1106 item 7), from the behaviour `docs/ctxdsl-modelling-guide.md` §3 documented as "true at every state".
- **Tests:** `x1106_an_atom_over_an_undeclared_variable_is_refused_naming_the_declared_ones` (the reproducer in the `ctxdsl` integration binary: compiles on the parent and fails there, where the typo atom evaluates `Ok` and holds).

## Not covered here

- A variable atom whose operands mix declared and undeclared names in different states (declared somewhere) keeps the `Maybe` reading; a per-state refusal is a separate question.
- The `bool` variable gap itself (no per-state valuation emitted): the refusal names it and the predicate route; emitting a valuation for `bool` variables is a modelling-guide follow-up.
