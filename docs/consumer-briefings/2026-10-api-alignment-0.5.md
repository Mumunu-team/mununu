# Consumer briefing — 0.5 API alignment: routes match the CLI verbs; the last unpinned responses get schemas (2026-10)

> Source of truth: [`crates/mununu-core/src/api/server.rs`](../../crates/mununu-core/src/api/server.rs) and [`crates/mununu-core/src/api/schema.rs`](../../crates/mununu-core/src/api/schema.rs) — surface: (API+UI)

**Audience:** mununu-ui above all (its typed client calls these routes), plus every HTTP client. Last structural cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.2 finding V4, §2.3 finding R4). Routes stay under `/api/v1`, as decided.

## TL;DR

- **The routes now carry the CLI verbs' names.** There are no aliases; the old paths are 404s.

  | before | after | CLI peer |
  |---|---|---|
  | `POST /api/v1/context/verify` | `POST /api/v1/context/eval` | `context eval` |
  | `POST /api/v1/context/synthesize` | `POST /api/v1/context/synth` | `context synth` |
  | `POST /api/v1/context/graphs` | `POST /api/v1/context/graph` | `context graph` |
  | `POST /api/v1/verify/memory-check` | `POST /api/v1/memory/check` | `memory check` |
  | `POST /api/v1/synth/gr1` | `POST /api/v1/context/synth` with `options.controller_mode: "gr1"` | `context synth --controller-mode gr1` |

- **GR(1) synthesis is one more `controller_mode` of `/context/synth`.** The request takes the TLSF spec as `context`, plus the new optional `adapter` (default `"tlsf"`) and `module` (default `gr1_controller`); `automaton` is optional for this mode. The answer is the synthesis response: `outcome`, `realizable`, the emitted SystemVerilog in `controller_native` (`<module>.sv`), and the game size and notes in `diagnostics.messages`. The request and response bodies of the other routes are unchanged.
- **Four new response schemas** under `docs/api-schemas/`, drift-tested like the others: `context-eval-response` (also `context eval --json`), `context-synth-response`, `btor2-game-response` and `cegar-response` (`/btor2/cegar`, `/sv/cegar`). Every response that decides a property is now schema-pinned.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **mununu-ui** | `endpoints.ts` calls `/context/verify`, `/context/synthesize`, `/context/graphs`, `/verify/memory-check` and `/synth/gr1` | rename the paths; send GR(1) as `/context/synth` with `controller_mode: "gr1"` and read the SV from `controller_native.content` |
| **rosf** | not an API consumer | none |
| **monono** | not an API consumer | none |

In this repo the wiki (API reference, Controller-Modes, Counterstrategy, Web-UI guide, CLI reference), the docs, the example READMEs and the `parity-check` skill follow.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the routes live in the binary |
| `mununu/docker/Dockerfile.dev` | yes, for the UI's dev backend | mununu-ui calls these routes |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | only if a lane uses the HTTP API | routes renamed |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts use the CLI |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction routes unchanged |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | CLI consumer |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | CLI consumer |

## Verification steps for every consumer

```bash
curl -s localhost:8080/api/v1/context/eval -H 'Content-Type: application/json' -d @eval.json | jq .outcome
curl -s localhost:8080/api/v1/context/synth -H 'Content-Type: application/json' \
  -d '{"context": {"name": "rg.tlsf", "content": "…"}, "options": {"controller_mode": "gr1"}}' | jq '.outcome, .controller_native.name'
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8080/api/v1/context/verify   # 404
```

## Provenance

- Branch `feat/api-alignment-0.5`. Tests: `gr1_synthesis_runs_on_the_synth_route` (a request without `automaton`, `outcome: "holds"`, the controller in `controller_native`, the game size in the messages), the handler suites under all features, and the drift tests for the four new schemas (generated with `MUNUNU_UPDATE_API_SCHEMAS=1`). The schemas needed `JsonSchema` on the strategy, counterstrategy-graph, CEGAR and sanity types, derived under the `api` feature only for the core ones.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The consumer migration PRs (rosf, monono, mununu-ui) and the 0.5.0 release with its consolidated migration briefing.
