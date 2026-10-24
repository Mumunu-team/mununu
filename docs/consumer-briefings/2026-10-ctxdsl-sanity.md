# Consumer briefing — `context eval --sanity`: structural checks on a CTXDSL model (2026-10)

> Source of truth: [`sanity::check`](../../crates/mununu-core/src/context/sanity.rs) — surface: (CLI+API+UI) — `context eval --sanity`, `sanity: true` on `POST /api/v1/context/verify`, the mununu-ui model sanity panel

**Audience:** monono, mununu-ui and anyone who authors CTXDSL models.

## TL;DR

- **New, opt-in:** `mununu context eval MODEL --automaton A --sanity` reports, on the realized automaton or composition: reachable deadlocks, each with a shortest initial → deadlock path; declared states never reached (for a composition, member states no reachable product state carries); labels that never fire; cycles made of internal labels only. With no `--formula` / `--template`, only the checks run.
- **API:** `sanity: true` on `/context/verify` adds `sanity: [SanityReport]` (one per checked automaton: `automaton`, `reachable_states`, `deadlocks[{states, labels}]`, `unreachable_states`, `labels_never_fired`, `internal_livelocks`). Absent otherwise; no other shape change.
- **UI:** the editor requests it on every verify and shows a "Model sanity" panel above the verdict table (mununu-ui PR).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (RTL only) | none |
| **monono** | optional new check for the CTXDSL lanes | consider adding `--sanity` to model lanes |
| **mununu-ui** | new request/response fields | merge the companion UI PR |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, to serve `sanity` | new API field |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | RTL image |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not realize CTXDSL |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | opt-in |

## Measured

| run | result |
|---|---|
| `context::sanity` tests | one automaton: the terminal `Done` is a deadlock with path `Init -go-> Busy -stop-> Done`, an orphan state is unreachable, a declared-only label never fires, an internal self-loop is a livelock; a two-state cycle is clean |
| the dead-vector model of mununu#570 with `allow_dead_vectors;` | 1 reachable state, deadlock `S\|P`, `Owner.T` unreachable, labels `a`, `b`, `c` never fire — the frozen automaton made visible |
| `context_verify_reports_sanity_on_request` (API) | the report rides beside the verdicts |

## Provenance

- Commit: pending merge — branch `feat/ctxdsl-sanity`; UI: mununu-ui `feat/context-sanity-panel`.
- Docs: [`wiki/CLI-Reference.md`](../../wiki/CLI-Reference.md), [`docs/ctxdsl-modelling-guide.md`](../ctxdsl-modelling-guide.md) checklist item 0.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The RTL counterpart (`check-fsm --liveness`) — the next PR.
- Completion states: an intended terminal state is reported as a deadlock; a property decides whether it is one.
