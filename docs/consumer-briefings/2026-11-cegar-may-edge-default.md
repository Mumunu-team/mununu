# Consumer briefing — the API's CEGAR routes default to the sound may-relation (2026-11)

> Source of truth: [`run_cegar_build_response`](../../crates/mununu-core/src/api/handlers.rs) — surface: API-only — `POST /api/v1/btor2/cegar` and `POST /api/v1/sv/cegar`; the CLI (`btor2 cegar`, `sv cegar`) already defaulted to the sound relation.

**Audience:** any client of the API's CEGAR routes, including mununu-ui's CEGAR runners.

## TL;DR

- **A soundness fix that changes verdicts.** With `may_edge_inference` absent, the API used `off`, a sampled may-relation that simulates one canonical representative per cube. That relation is an under-approximation that can miss a may-successor, so a definite HOLDS on a box property (`[]`) could be wrong (mununu#867). The response carried the A.4 warning saying so, but the verdict was definite.
- **Now:** absent means `smt-all-pairs`, the sound all-pairs SMT may-relation, which is the CLI's default. A cell the sampled relation proved TRUE by missing a path now comes back FALSE or undecided (⊥).
- **Opting out:** `"off"` is still accepted, with the A.4 warning, for a client that wants the faster relation and reads the warning.
- **Cost:** O(cubes²) SMT calls instead of one simulation per cube. It is tractable at the cube counts the routes accept (the CLI has paid it by default since AR-S2). No timing comparison was made for this change.
- **No JSON shape change.**

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **mununu-ui** | the CEGAR and SV-CEGAR runners send `may_edge_inference: "off"` explicitly (their dropdown default), so they keep the sampled relation and its warning | change the dropdown default to `smt-all-pairs` (companion PR) |
| **rosf** | none known: no CEGAR call site | none |
| **monono** | none known: no CEGAR call site | none |
| **other HTTP clients** | a request without `may_edge_inference` now gets sound verdicts, possibly more ⊥, and takes longer on large cube counts | if you relied on `off`, pass it explicitly and read `warnings` |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, if clients call the CEGAR routes | the default is in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow calls the CEGAR routes | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no for CLI use (the CLI is unchanged); yes if you serve the API from it | — |
| `mununu/docker/Dockerfile.profile` | no | profiling image |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not serve the API |
| rosf / monono images | no | no CEGAR call site |

## Provenance

Reproducer: `crates/mununu-core/tests/cegar_may_edge_default_867.rs`.

- **The design.** A 2-bit `x` steps `0 → 2 → 3`, with `bad = (x == 3)`, so the property is violated at depth 2. The predicates are `x0`, `x3`, and the formula is `nu X. ((! x3) && ([] X))`.
- **On the parent** (default `off`): the initial cell is a definite TRUE. Only the `x3` cells are violating, and the A.4 warning is attached.
- **On the fix** (default `smt-all-pairs`): the initial cell is undecided, which is sound.
- **Measured** 2026-11-06 through `btor2_cegar_handler`.

## Not covered here

- The import route's controllability-aware predicate lift also uses the sampled relation. It returns only a summary (cube and edge counts, a comment-only CTXDSL) and no verdict, so no unsound verdict leaves through it. Its counts describe the sampled relation.
- `"off"` stays available with the A.4 warning; it is not removed.
