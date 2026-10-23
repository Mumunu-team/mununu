# Consumer briefing — CTXDSL guards take `&&`, `||`, `!`; an undecidable guard is an error (2026-10)

> Source of truth: [`parse_guard_tree`](../../crates/mununu-core/src/guard/mod.rs) + [`UnrollingError::UndecidableGuard`](../../crates/mununu-core/src/abstraction/unrolling.rs) — surface: (CLI+API+UI) — every path that realizes a CTXDSL automaton (`context eval|synth|graph|summarize`, `verify`, `/api/v1/context/*`)

**Audience:** monono, mununu-ui and anyone who authors CTXDSL models with `variables` and `guard`s.

## TL;DR

- **Verdict change (bugfix).** A guard with `&&`, `||` or `!` was split at its first comparison operator and silently disabled its transition; so was a bare boolean-variable guard (`guard ready`). Those transitions now fire when their guard holds, so states that were unreachable can become reachable and verdicts over them can change.
- **New error.** A guard the variables cannot decide — typically a name that is no declared `i64` / `bool` variable (`guard cuont >= 1`) — is now a realization error naming the transition, instead of a transition that never fires.
- No flag, field or schema changes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (RTL only) | none |
| **monono** | `context eval` / `verify` verdicts on models with compound or boolean-variable guards may change; a model with a misspelled guard variable now fails to realize | re-run the CTXDSL model lanes; fix any `UndecidableGuard` the run reports |
| **mununu-ui** | the same verdicts over `/api/v1/context/*`; realization errors surface as API errors | none required |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the fix lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | RTL image; CTXDSL realization is the same binary, rebuild with the next RTL change |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not realize CTXDSL |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | CTXDSL model lanes |

## Measured

| run | result |
|---|---|
| `a_compound_guard_constrains_its_transition_instead_of_disabling_it` | `Open` unreachable on the parent (`a >= 2 && b == 0` never fired), reachable here; `make verify-repro` CONFIRMED |
| `realize_marks_uncontrollable_transitions` | the `guard is_controllable` loop (variable `true`) did not exist on the parent; it exists and is controllable here (the test no longer hedges on it) |
| `cargo test -p mununu-core --no-fail-fast` | all 55 test binaries pass |
| compound guards in the shipped models (`examples/**`, mununu-ui `public/examples`) | none (30 guards, all single comparisons) — no shipped verdict changes |

## Provenance

- Fix commit: pending merge — branch `fix/ctxdsl-guard-expressions`.
- Docs: [`docs/ctxdsl-modelling-guide.md` §1](../ctxdsl-modelling-guide.md), [`wiki/CTXDSL-Language-Reference.md`](../../wiki/CTXDSL-Language-Reference.md).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Refinement still splits only `i64` intervals against constants; a guard comparing two variables over wide intervals can still be undecidable at a state, and is now reported rather than dropped.
