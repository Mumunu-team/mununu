# Consumer briefing — an unmatched synchronisation vector is a CTXDSL realization error (2026-10)

> Source of truth: [`dead_compound_actions`](../../crates/mununu-core/src/composition/mod.rs) + `CompositionOptions::dead_vectors_are_errors` — surface: (CLI+API+UI) — every path that realizes a CTXDSL composition (`context eval|synth|graph|summarize`, `/api/v1/context/*`)

**Audience:** monono, mununu-ui and anyone who authors or generates CTXDSL compositions with multi-label transitions.

## TL;DR

- **Behaviour change.** A compound action (`on {label a, label b}`, or the legacy `on label a, label b`) that no partner transition can match over the shared alphabet freezes its automaton (mununu#570). That used to be a `tracing::warn` that `--quiet` hid; realizing such a CTXDSL composition is now an error naming the vector and its shared projection.
- **Opt-in.** A composition that disables the action on purpose declares `allow_dead_vectors;` after its `members` clause and keeps the old warning.
- Compositions built by the adapters (yosys multi-module, `verify.toml` assembly) keep the warning.
- No flag, field or schema changes; the DSL gains one composition statement.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (RTL only) | none |
| **monono** | a CTXDSL model with a dead vector now fails to realize instead of verifying a frozen automaton | fix the vector (`a \| b` for alternatives, or a matching joint partner action), or add `allow_dead_vectors;` |
| **mununu-ui** | the error arrives as an API error; editors accept the new statement | surface the message; no client change required |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the check lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | RTL image |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not realize CTXDSL |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | CTXDSL model lanes |

## Measured

| run | result |
|---|---|
| `an_unmatched_synchronisation_vector_is_a_realization_error` | realizes on `main` (frozen automaton, no error); an error here; `allow_dead_vectors;` realizes it |
| shipped models (`examples/**`, `crates/mununu-core/tests/fixtures/**`, mununu-ui `public/examples/**`) realized with `context summarize` | 153 models, 0 dead-vector errors |
| `tests/adapter_xstate_viability.rs::viability_variable_automaton_guard_sync` | its hand-built IR carried two dead vectors (`Machine` froze in `Working`; the test asserted only `state_count() > 0`); the counter automaton now offers the joint test-and-set edges and the test asserts `Done` is reachable |

## Provenance

- Fix commit: pending merge — branch `fix/ctxdsl-dead-sync-vectors`.
- Docs: [`docs/abstraction.md`](../abstraction.md) §"Multi-label transitions are synchronisation vectors", [`wiki/CTXDSL-Language-Reference.md`](../../wiki/CTXDSL-Language-Reference.md).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The check matches projections against the partner's transitions anywhere in the automaton, not at reachable states; a vector whose matching partner edge is itself unreachable is still not reported.
- The structured `diagnostics.dead_vectors` field for the opt-in case (the warning remains log-only there).
