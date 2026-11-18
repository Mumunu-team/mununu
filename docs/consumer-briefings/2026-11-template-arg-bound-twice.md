# Consumer briefing: a template argument bound twice is refused (2026-11)

> Source of truth: [`parse_template_clause`](../../crates/mununu-core/src/context_dsl/parser.rs) and [`template_refs`](../../crates/mununu-cli/src/main.rs) — surface: CLI+API — every path that parses a CTXDSL `template` clause (CLI, HTTP API, UI); `--template-arg` on the CLI

**Audience:** anyone who writes CTXDSL `template` clauses or passes `--template-arg`.

## TL;DR

- **A key bound twice is now an error (mununu#966).** Until now the later value silently replaced the earlier, so the property checked was not the one the first binding named.
- **CTXDSL:** `template reachable { TARGET = S; TARGET = T; }` is a parse error, for example `template argument `TARGET` is bound twice (line 4, then line 5)`.
- **CLI:** `--template-arg TARGET=S --template-arg TARGET=T` is refused, naming the key. The same key on two different templates (`reachable.TARGET=… unreachable.TARGET=…`) is two bindings, as before.
- **No verdict changes** for a file or call that binds each key once.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **mununu-ui** | A CTXDSL document with a repeated template key now fails to parse; the editor shows the parse error. | None, unless your generated models repeat a key: then bind it once. |
| **rosf** | Same, for any CTXDSL or `--template-arg` it generates. | Same. |
| **monono** | Same. | Same. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the refusal. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the full tier-B review, 2026-11-09 (#966).
- **Tests:** `a_template_argument_bound_twice_is_refused_not_overwritten` (CTXDSL, fails on the parent) and the CLI's `a_template_arg_bound_twice_is_refused`.

## Not covered here

- The HTTP API's `template_ref.args` / `templates[].args` are JSON objects. A JSON object with a repeated key is still read by serde with the last value kept; refusing it needs a custom deserializer and stays open on #966.
- SV `@mununu_template` annotations already refused a repeated key (`` `KEY` is given twice``).
