# Consumer briefing — `fairness weak;` on asynchronous compositions; the async soundness note corrected (2026-10)

> Source of truth: [`translate_universal_fair`](../../crates/mununu-core/src/ltl/universal.rs) + [`Assumption::WeakEdges`](../../crates/mununu-core/src/verify/fairness.rs) — surface: (CLI+API+UI) — every path that realizes a CTXDSL document

**Audience:** monono, mununu-ui and anyone who checks liveness on an `asynchronous` composition.

## TL;DR

- **New, opt-in:** `asynchronous Sys { members [A, B]; fairness weak; }` reads `ltl` bodies over the composition's fair paths — every member whose moves stay enabled eventually takes one. A liveness property violated only by a starving schedule then holds; a genuine violation stays violated.
- **Scope:** `ltl` bodies whose formula targets the fair composition alone (`F b` becomes the Emerson–Lei fair `AF b`; `b U c` becomes `(b W c) ∧ fair AF c`; `G` / `W` unchanged — weak fairness is machine-closed). A formula targeting the fair composition together with other automata is refused. `eltl` and raw μ-calculus are evaluated as written. `fairness weak;` on a `synchronous` composition is a parse error.
- **Documentation fix:** the asynchronous composition was described as "unsound for liveness" and as letting independent actions "proceed jointly". Both were wrong: a universal property that holds over the interleaving product holds under any scheduler (starvation only produces spurious violations), and independent actions step jointly only when they complete a declared synchronisation vector.
- No change to any existing verdict: the option is new and off unless declared.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (RTL only) | none |
| **monono** | none unless you adopt `fairness weak;` | optional: declare it on asynchronous compositions whose liveness lanes fail by starvation |
| **mununu-ui** | the editor should accept the `fairness weak;` statement | add it to the CTXDSL highlighter |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and want the option | new DSL statement |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | RTL image |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not realize CTXDSL |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | opt-in |

## Measured

| run | result |
|---|---|
| `weak_fairness_removes_starvation_but_not_a_genuine_violation` (`context/mod.rs`) | `ltl F a_done` over `asynchronous [A, B]` where `B` may loop forever: violated without fairness, holds with `fairness weak;`; with a dead-end branch in `A`, violated under fairness too |
| `cargo test -p mununu-core --no-fail-fast` | all 55 test binaries pass |

## Provenance

- Commit: pending merge — branch `feat/async-fairness` (on top of the `ltl` path-semantics fix, mununu#764).
- Docs: [`wiki/Composition.md`](../../wiki/Composition.md) §"Liveness over interleavings, and `fairness weak;`".
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Strong fairness, and fairness for raw μ-calculus bodies (the `verify.toml` `[[assumptions]]` path covers those).
