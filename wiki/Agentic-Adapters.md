# Agentic Adapters

> **Source of truth:** [`crates/mununu-core/src/adapter/crewai/`](https://github.com/Mumunu-team/mununu/tree/main/crates/mununu-core/src/adapter/crewai/), [`crates/mununu-core/src/adapter/langgraph/`](https://github.com/Mumunu-team/mununu/tree/main/crates/mununu-core/src/adapter/langgraph/) — surface: CLI+API+UI.

Mununu ships **native parsers** for CrewAI and LangGraph workflow exports. Drop a `.crewai.json` / `.langgraph.json` file into the CLI, the HTTP API, or the UI wizard and the corresponding adapter translates it into CTXDSL automata directly — no manual rewrite into XState required.

For broader context on agentic verification (counterexample interpretation, MCP authorization patterns, `__mununu` annotation syntax), see [Agentic Orchestration](Agentic-Orchestration). This page focuses on the **format adapters**.

## CrewAI adapter

> **Source of truth:** [`adapter::crewai::CrewaiAdapter`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/crewai/mod.rs) — surface: CLI+API+UI.

### What it accepts

CrewAI v0.50+ `Crew` JSON exports. Two top-level shapes:

```json
{
  "name": "ResearchAndWrite",
  "agents": [{ "role": "Researcher", "goal": "..." }, { "role": "Writer" }],
  "tasks":  [{ "agent": "Researcher", "expected_output": "..." }],
  "process": "sequential"
}
```

or wrapped:

```json
{ "crew": { "name": "...", "agents": [...], "tasks": [...] } }
```

Detection runs on file extension (`.crewai.json` / `.crewai`) and on content (`agents` + `tasks` OR `agents` + `crew` envelope).

### How it translates

> **Source of truth:** [`adapter::crewai::translate::to_ir`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/crewai/translate.rs) — surface: CLI+API+UI.

- **One automaton per agent.** States: `Idle -> Executing -> Done -> Idle`. Labels: `agent_<role>_start` (controllable — the supervisor dispatches), `agent_<role>_complete` (uncontrollable — the LLM completes when it completes).
- **One supervisor automaton.** States `Init -> AfterTask_1 -> ... -> AfterTask_N` driven by `agent_<role>_complete` events in declared task order.
- **Asynchronous composition** over `[supervisor, agent_1, ..., agent_N]` per the [agentic adapter soundness notes](https://github.com/Mumunu-team/mununu/blob/main/docs/adapters/agentic.md) — LLM latency is non-deterministic, so synchronous one-step rendezvous is unsound for liveness without explicit fairness.
- **`__mununu` block overrides** controllability classifications via the same convention as the XState adapter.

`process = "sequential"` is fully modelled; `hierarchical` and `consensual` emit an `ApproximateTranslation` warning and fall back to sequential. Native support for those processes is a planned follow-up.

### Process discipline support

| `process` | Behaviour |
|---|---|
| `sequential` | Full support — supervisor enforces declared task order |
| `hierarchical` | Falls back to sequential; emits an `ApproximateTranslation` warning |
| `consensual` | Falls back to sequential; same warning |

## LangGraph adapter

> **Source of truth:** [`adapter::langgraph::LangGraphAdapter`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/langgraph/mod.rs) — surface: CLI+API+UI.

### What it accepts

LangGraph `StateGraph` JSON exports:

```json
{
  "name": "TicketTriage",
  "entry_point": "classify",
  "nodes": [
    { "id": "classify", "kind": "agent" },
    { "id": "billing", "kind": "agent" },
    { "id": "done", "kind": "end" }
  ],
  "edges": [
    { "from": "classify", "to": "billing", "condition": "is_billing" },
    { "from": "billing", "to": "done" }
  ]
}
```

Three accepted shapes: flat `{nodes, edges}` (canonical), wrapped `{graph: {nodes, edges}}`, and the `nodes` field as either an array or an object map (`id -> spec`).

### How it translates

> **Source of truth:** [`adapter::langgraph::translate::to_ir`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/langgraph/translate.rs) — surface: CLI+API+UI.

- **One state per node.** The entry-point node (or the first node if absent) becomes the initial state.
- **One transition per edge** with label `node_<from>_enter`. Conditional edges get a per-condition suffix: `node_<from>_<condition>_enter`.
- **Controllability default**: node-enter labels emitted from non-`end` source nodes are **controllable** (the scheduler picks the next transition). Labels from `kind = "end"` source nodes are **uncontrollable** (the runtime decides when to terminate). `__mununu` overrides win in either direction.
- **No auto-composition.** Composition with other sources is left to the verify-framework manifest, not the adapter.

## Property templates for agentic flows

> **Source of truth:** [`adapter::templates::builtin_templates`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/templates/builtin_templates.json) — surface: CLI+API+UI.

Three templates tagged `agentic` in the builtin catalog cover the most common verification questions:

| Template | Kind | What it checks |
|---|---|---|
| `bounded_handoff(HANDOFF_TRIGGERED, HANDOFF_COMPLETE)` | liveness | Once the handoff is triggered, completion is reachable from there |
| `no_delegation_cycle(FORWARD, BACKWARD)` | safety | After a forward delegation, the reverse delegation never fires (catches A→B→A cycles) |
| `eventual_completion(TASK_STARTED, TASK_DONE)` | liveness | Every started task reaches a terminal state — LangGraph reachability of `END` / CrewAI task settlement |

The full universal catalogue (`no_deadlock`, `reachable`, `never`, `mutual_exclusion`, `bounded`, `response`, `label_blocked_in_state`, `no_clobber`, `clobber_reachable`, `mutual_exclusion_3`, `no_lost_update`) also applies — see [Property Templates](Property-Templates).

## CLI

```bash
# Auto-detected from extension
mununu context eval crew.crewai.json --formula safety --automaton Agent_Researcher
mununu context eval graph.langgraph.json --formula safety --automaton TicketTriage

# Explicit adapter
mununu context eval crew.json --adapter crewai --formula safety
mununu context eval graph.json --adapter langgraph --formula safety
```

## HTTP API

> **Source of truth:** [`api::handlers::context_import_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

`POST /api/v1/context/import` accepts `format = "crewai"` or `format = "langgraph"` in addition to the legacy `auto | xstate | systemverilog | sv-yosys | tlsf | aiger | btor2 | promela | extraction` set.

```bash
curl -X POST http://127.0.0.1:8080/api/v1/context/import \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg c "$(cat crew.crewai.json)" \
        '{content: $c, format: "crewai", filename: "crew.crewai.json"}')"
```

## Web UI

> **Source of truth:** [`mununu-ui/src/types/workflow.ts`](https://github.com/Mumunu-team/mununu-ui/blob/main/src/types/workflow.ts) — surface: UI.

The Extraction tab's domain selector exposes **CrewAI Agentic** and **LangGraph Workflow** as dedicated wizards. Drop a `.crewai.json` / `.langgraph.json`, click **Run Translate**, then switch to the Verification tab to evaluate properties on the emitted CTXDSL.

## End-to-end examples

> **Source of truth:** [`examples/verify/crewai_handoff/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/crewai_handoff/), [`examples/verify/langgraph_workflow/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/langgraph_workflow/) — surface: CLI.

Two verify-framework fixtures exercise the adapters end-to-end through `mununu verify`:

```bash
bash examples/verify/crewai_handoff/validate.sh
bash examples/verify/langgraph_workflow/validate.sh
```

Each ships a `verify.toml`, the source JSON, a `validate.sh`, and a byte-deterministic `transcript.txt`. They make good copy-paste starting points for your own crews and graphs.

## What's not yet supported

> **Status: planning** — these are queued follow-ups, not shipped features.

- **A2A protocol** and **AutoGen JSON** — both have JSON serialisations but enough surface-shape difference from CrewAI / LangGraph to warrant separate adapters.
- **CrewAI hierarchical / consensual** processes — currently fall back to sequential with a warning.
- **LangGraph state-schema lifting** to per-state predicates — the state schema is preserved on the AST but unused by today's translator.
- **Runtime Python introspection** (e.g. `tools/crewai_extract.py` walking live `Crew` objects) — the JSON-export path is sufficient since both frameworks already serialise their graphs.

## See also

- [Agentic Orchestration](Agentic-Orchestration) — verification patterns, MCP authorization, counterexample interpretation
- [Verify Project Flow](Verify-Project-Flow) — the general N-source framework these adapters plug into
- [Adapter Formats](Adapter-Formats) — every format adapter shipped with mununu
- [Property Templates](Property-Templates) — the full template catalogue, including the three agentic-specific entries
