# The MCP server — `mununu mcp`

> Source of truth: [`api::mcp::MununuMcp`](../crates/mununu-core/src/api/mcp.rs) + [`Commands::Mcp`](../crates/mununu-cli/src/main.rs) — surface: CLI+API — the MCP tool is the HTTP handler's op (`api::ops::context_verify`, `api::ops::templates`); the CLI subcommand starts the server.

`mununu mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io/specification/2026-07-28)
server over stdio. It is a **third transport of the HTTP API, not a new capability**: the one tool that
decides anything takes the body of `POST /api/v1/context/eval` and returns that route's response, byte
for byte — one shape, two surfaces, as mununu#536 did for `sv verify-auto --json` and the HTTP body. An
agent that can call MCP tools can hand mununu CTXDSL sources and read the verdicts without a shell, a
server process or a file on disk.

`engine: unchanged — the tool calls the same CTXDSL evaluation the HTTP handler calls (explicit μ-calculus
over the realized CLTS; the counterstrategy by formula inversion); the transport is the only new code.`

## The two tools

| tool | mirrors | input | output |
|---|---|---|---|
| `context_eval` | `POST /api/v1/context/eval` | the route's request body ([`context-eval-request.schema.json`](api-schemas/context-eval-request.schema.json)): `context` (the CTXDSL file, inline), `sidecars`, `formula` or `template_ref`, `automaton`, `counterstrategy`, `minimize_counterstrategy`, `hide`, `minimize`, `stubs` (these three are accepted and NOT applied today, mununu#862), `sanity` | `structuredContent` = the route's response ([`context-eval-response.schema.json`](api-schemas/context-eval-response.schema.json)), `outputSchema` advertised from the same type; one text block with the compact summary — the `outcome`, then one line per result, `formula_name @ automaton: outcome` |
| `templates` | `GET /api/v1/templates` | `{ "domain": "rtl" \| "agentic" \| "software" \| "synthesis" \| "universal" \| null }` | `structuredContent` = the catalogue (or the domain's templates), the same JSON the route returns; read-only |

Sources come **inline** in `context.content` (and `sidecars[].content`), never as paths: the server opens
no file on the host, exactly like the HTTP model. The advertised `inputSchema` and `outputSchema` of
`context_eval` are the two committed schema files, and a unit test keeps them identical.

Nothing else is exposed. The `sv` and `btor2` verbs and `context/synth|graph|import|predicates` follow
the same recipe (an `api::ops` function behind a tool) and are listed as follow-ups in the PR that added
this server; they are not built here.

## Errors and the budget

A design problem — a CTXDSL syntax error, an unknown formula or automaton, a template that does not
instantiate — is a **tool result with `isError: true`**, never a protocol-level JSON-RPC error. Its text
and its `structuredContent` are the same `{ "success": false, "error": { "code", "message", "details" } }`
body the HTTP 400 carries, so an agent reading either surface sees one message.

Every call runs on a blocking thread under a wall-clock budget, `--request-timeout <seconds>`, default
30 — the HTTP layer's `TimeoutLayer` value. A call over budget returns `isError: true` with the budget
named in the message (`the request exceeded its budget of 30000 ms`); the engine thread finishes on its
own in the background, as a timed-out HTTP request's does. The budget is a transport setting: it
changes what the tool *reports*, never what the engine *decides*.

## Client configuration

Claude Code:

```sh
claude mcp add mununu -- mununu mcp
# with a budget and a deployment configuration (template packs, enabled domains — mununu#715):
claude mcp add mununu -- mununu mcp --request-timeout 120 --config /etc/mununu/mununu.toml
```

Any MCP client, stdio transport:

```json
{
  "mcpServers": {
    "mununu": {
      "command": "mununu",
      "args": ["mcp", "--request-timeout", "60"]
    }
  }
}
```

The `mununu` binary must be on the client's `PATH` (or give the absolute path). Under stdio, **stdout is
the protocol channel**: every mununu log line goes to stderr, and `RUST_LOG` still filters them.

A `--config mununu.toml` is installed before the server starts, the same way `mununu server --config`
does it: the template packs it names are what `templates` lists and what `template_ref` can
instantiate, and every report the process produces carries the deployment identity.

## A call, end to end

```json
{
  "name": "context_eval",
  "arguments": {
    "context": { "name": "counters.ctxdsl", "content": "context counters { alphabet { label tick; } automata { automaton Counter { states { state Idle initial; state Active; } transitions { transition Idle -> Active on label tick; transition Active -> Active on label tick; } } } }" },
    "sidecars": [ { "name": "props.ctxdsl", "content": "context props { mu_formulas { formula reachability { over Counter; body = mu X. ( < labels = { tick } > X || < labels = { tick } > true ); } } }" } ],
    "formula": "reachability"
  }
}
```

returns the text `holds` / `reachability @ Counter: holds` and, as `structuredContent`, exactly what
`curl -X POST localhost:8080/api/v1/context/eval` returns for the same body:

```json
{ "success": true, "outcome": "holds", "results": [ { "formula_name": "reachability", "automaton": "Counter", "outcome": "holds", "total_states": 2, "satisfying_states": 2, "initial_states": ["Idle"], "initial_satisfying": ["Idle"], "initial_violating": [], "satisfying_state_names": ["Active", "Idle"] } ] }
```

## Posture

- **Local only.** The stdio transport has no network surface at all. A streamable-HTTP transport
  (`/mcp` under `mununu server`) is a follow-up; if it lands it binds `127.0.0.1` by default like the
  HTTP server.
- **No file access.** The server reads nothing from the host's file system on a client's behalf.
- **No new verdict path.** The tool and the route share one function; the MCP module carries no
  verification logic. MCP is a third transport of the API, so its surface tag is `CLI+API`
  (the `mununu mcp` subcommand starts it; the tool is the HTTP handler's op).

## Out of scope today

- Streamable HTTP (`--transport http`) — follow-up; the subcommand accepts `stdio` only.
- The `sv` / `btor2` verbs, `context/synth`, `context/graph`, `context/import`, `context/predicates`
  as tools — same recipe, not built.
- Resources and prompts: the server advertises tools only.
