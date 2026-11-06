# Consumer briefing — `context/eval` refuses `hide`, `minimize` and `stubs` instead of ignoring them (2026-11)

> Source of truth: [`ContextVerifyRequest::unsupported_fields`](../../crates/mununu-core/src/api/models.rs) + [`ops::context_verify`](../../crates/mununu-core/src/api/ops.rs) — surface: CLI+API — `POST /api/v1/context/eval` and the MCP tool `context_eval` (started by `mununu mcp`); `mununu context eval` (CLI) never had these options.

**Audience:** any HTTP client of `POST /api/v1/context/eval`, including mununu-ui.

## TL;DR

- **A silence fix with one behaviour change.** The request type declared `hide`, `minimize` and `stubs`, and the route never read them (mununu#862). A request that set one succeeded, and its verdicts were computed on the unhidden, unminimised, unstubbed model with nothing in the response saying so.
- **Now:** a request that sets any of them gets **400 `BAD_REQUEST`** with the message `context/eval does not apply <fields>; remove the field(s) from the request (mununu#862)`. A request that leaves them at their defaults (`[]`, `false`, `[]`, or absent) is unchanged.
- **No JSON shape change and no verdict change** for a request that was evaluated as asked.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **mununu-ui** | none: the UI declares the three fields in its request type but never sends them to this route (it sends `minimize` only to `context/synthesize`, which applies it) | none; the fields can be dropped from `ContextVerifyRequest` in `src/api/endpoints.ts` |
| **rosf** | none known: rosf does not call `context/eval` | none |
| **monono** | none known: monono does not call `context/eval` | none |
| **other HTTP clients** | a request that set one of the three now fails instead of returning verdicts on a different model | remove the field; to hide labels or minimise, change the CTXDSL source |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if a client sets one of the three fields | the route's behaviour is in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow calls `context/eval` with these fields | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | the SVA image does not serve `context/eval` in any consumer flow |
| `mununu/docker/Dockerfile.profile` | no | profiling image |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not serve the API |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not call the route |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | monono does not call the route |

## Provenance

Found in the review of the MCP server PR (#863), which advertises this request type's schema to agents: the route's body never reads the three fields. Reproducer: `crates/mununu-core/tests/context_eval_unsupported_fields_862.rs`. On the parent each field is accepted and the request evaluated; on the fix each is refused with its name. A positive control checks that the same request without them is evaluated.

## Not covered here

Implementing `hide`, `minimize` or `stubs` for this route. The CLI has no peer and nobody has asked for them; a client that needs one can change the CTXDSL source. The MCP tool `context_eval` (#863) calls the same op, so it refuses the three fields the same way, as a tool result with `isError`.
