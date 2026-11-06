# Consumer briefing — `mununu mcp`: a Model Context Protocol server, CTXDSL sources in, verdicts out, the `/api/v1/context/eval` shapes (2026-11)

> Source of truth: [`api::mcp::MununuMcp`](../../crates/mununu-core/src/api/mcp.rs) + [`Commands::Mcp`](../../crates/mununu-cli/src/main.rs) — surface: CLI+API — the MCP tool is the HTTP handler's op; new CLI subcommand `mununu mcp`.

**Audience:** rosf, monono, mununu-ui, and anyone who drives mununu from an agent (Claude Code, any MCP client). A surface add (a CLI subcommand, a new transport of an existing route); no route, flag or verdict of the existing surfaces changes.

## TL;DR

- **New subcommand `mununu mcp`** (`--request-timeout <seconds>`, default 30; `--config mununu.toml`; `--transport stdio`, the only value today). It serves two MCP tools over stdio: `context_eval` and `templates`.
- **`context_eval` = `POST /api/v1/context/eval`, byte for byte.** Its `inputSchema` is the request body's schema — new file [`docs/api-schemas/context-eval-request.schema.json`](../api-schemas/context-eval-request.schema.json), now under the drift test like the response — and its `structuredContent` is the route's response (`context-eval-response.schema.json`). One text block carries a compact summary (`outcome`, then `formula_name @ automaton: outcome` per result). Sources come inline; the server opens no file.
- **`templates` = `GET /api/v1/templates`**, read-only, optional `domain`.
- **Errors keep their shape.** A design problem (CTXDSL syntax error, unknown formula, a template that does not instantiate) is a tool result with `isError: true` whose text and `structuredContent` are the HTTP 400 body `{success:false, error:{code,message,details}}`; a call over the budget is `isError: true` naming the budget. Never a protocol-level error for a design problem.
- **One op, two transports.** The body of the HTTP handler moved to `api::ops::context_verify` (and `api::ops::templates`); the axum handler and the MCP tool both call it, so the two cannot drift. The seam test drives both in-process on the same request and asserts equality.
- **Nothing else changes.** No existing route, response, flag, exit code or verdict moves. The `ContextVerifyRequest` type gains a `schemars::JsonSchema` derive (and field docs) — a derive, no wire change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | nothing: rosf drives the `sv` verbs, which are not MCP tools (yet) | none required |
| **monono** | nothing: the lane scripts call the CLI | none required; a monono agent may add `claude mcp add mununu -- mununu mcp` to evaluate CTXDSL models without a server |
| **mununu-ui** | nothing: the UI speaks HTTP; `POST /api/v1/context/eval` is unchanged | none required; the request schema file is new and can type the UI's client (`endpoints.ts` already mirrors the request) |
| **agent users** | a new way in: `claude mcp add mununu -- mununu mcp` | see [`docs/mcp.md`](../mcp.md) for the client config and the budget |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, when you want the subcommand | the binary carries `mununu mcp`; nothing else in the image changes |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow runs the new binary | the binary |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | the SV lift and its tools are untouched; the binary lives in the cargo-target volume |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not serve |
| rosf images | no | rosf does not use the MCP surface |
| monono images | no | the engine comes from the cargo-target volume; no lane script changes |

## Measured

| run | result |
|---|---|
| seam test: `context_eval` vs `POST /api/v1/context/eval` on the same request (all formulas, one formula, `template_ref`, `counterstrategy`, `sanity`, a CTXDSL syntax error, a 1-ms budget) | identical on every case, plus the syntax error as an `isError` result with the same body and the 1-ms budget as an `isError` naming it (`api::mcp::tests`, 5 tests, in `make ci`) |
| schema identity: the tool's advertised `inputSchema` / `outputSchema` vs the two committed schema files | identical (`the_tools_advertise_the_api_schemas`) |
| per-call overhead, MCP vs HTTP, same example, same box | MCP 0.58 ms, HTTP 0.48 ms per call (in-process, 5 calls, optimized test profile); the ≈ 0.1 ms is JSON-RPC framing, under the 2× noise floor and immaterial beside the evaluation |

## Provenance

- Branch `feat/mcp-context-eval`, from `main` at `28289548`. PR #863.
- Dependency: `rmcp` 3.x (the official Rust MCP SDK; targets the 2026-07-28 revision), features `server` + `transport-io`, no macros: the tools are declared from the API's own `schemars` 0.8 schemas so the advertised schema is the committed file.
- Docs: [`docs/mcp.md`](../mcp.md), `wiki/API-Reference.md` §"MCP server", [`docs/api-schemas/README.md`](../api-schemas/README.md).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md) (a route/surface add fires a briefing).

## Not covered here

- Streamable HTTP (`/mcp` under `mununu server`): a follow-up; `--transport` accepts `stdio` only.
- The `sv` / `btor2` verbs and `context/synth|graph|import|predicates` as tools: same recipe, not built.
- Resources, prompts, sampling: the server advertises tools only.
