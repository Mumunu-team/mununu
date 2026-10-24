# Agentic Orchestration Examples

Models for verifying multi-agent / tool-orchestration workflows. Four entry points:

- **Native CTXDSL** — agent / supervisor / authorization gates as composed automata.
- **XState JSON** — workflows as XState v5 statecharts with a `__mununu` block (controllability + properties), consumed by the XState adapter.
- **CrewAI JSON** (`.crewai.json`) — sequential / hierarchical crews consumed by the native [`CrewaiAdapter`](../../crates/mununu-core/src/adapter/crewai/). Per-agent automata (`Idle -> Executing -> Done` on `agent_<role>_start` / `agent_<role>_complete`) + sequential supervisor + asynchronous composition.
- **LangGraph JSON** (`.langgraph.json`) — `StateGraph` workflows consumed by the native [`LangGraphAdapter`](../../crates/mununu-core/src/adapter/langgraph/). Nodes become states; conditional edges become `node_<from>_<condition>_enter` transitions.

A2A and AutoGen JSON are not yet wired natively; rewrite as XState, CrewAI, or LangGraph, or hand-author CTXDSL. End-to-end verify-framework fixtures for the agentic adapters live under [`examples/verify/crewai_handoff/`](../verify/crewai_handoff/) and [`examples/verify/langgraph_workflow/`](../verify/langgraph_workflow/).

## Top-level examples

### Customer Support Pipeline (XState)

`support_pipeline.xstate.json` — parallel triage + budget-tracking pipeline. Auto-detected when the file extension is `.xstate.json`.

```bash
# Eval (auto-detect from extension)
mununu context eval examples/agentic/support_pipeline.xstate.json \
    --formula no_tool_over_budget --automaton support_pipeline_system
mununu context eval examples/agentic/support_pipeline.xstate.json \
    --formula safety_invariant --automaton support_pipeline_system

# Synthesis
mununu context synth examples/agentic/support_pipeline.xstate.json \
    --adapter xstate \
    --formula no_tool_over_budget --automaton support_pipeline_system
```

The XState adapter appends `_system` to the automaton name when the machine has parallel regions, so the automaton here is `support_pipeline_system`, not `support_pipeline`.

### MCP Tool Authorization (Native CTXDSL)

`mcp_auth.ctxdsl` — Session × Confirmation composition (`auth_system`).

```bash
mununu context eval examples/agentic/mcp_auth.ctxdsl \
    --formula session_required --automaton auth_system
mununu context eval examples/agentic/mcp_auth.ctxdsl \
    --formula confirm_before_delete --automaton auth_system
mununu context eval examples/agentic/mcp_auth.ctxdsl \
    --formula can_reach_active --automaton auth_system
```

### Multi-Agent Handoff (Native CTXDSL)

`handoff_protocol.ctxdsl` — Supervisor + AgentA + AgentB composition (`handoff_system`).

```bash
mununu context eval examples/agentic/handoff_protocol.ctxdsl \
    --formula mutex --automaton handoff_system
mununu context eval examples/agentic/handoff_protocol.ctxdsl \
    --formula supervisor_completes --automaton handoff_system
mununu context eval examples/agentic/handoff_protocol.ctxdsl \
    --formula no_orphaned_tasks --automaton handoff_system
```

## Subdirectories

- `mcp_extracted/` — CTXDSL extracted from real MCP server source (`cve_2026_25536_*` and `mcp_streamable_http*` pairs). Use `mununu context summarize <file>` to discover the automaton and formula names per file.
- `mcp_usecases/` — Hand-authored CTXDSL covering common MCP patterns (tool chainer, OAuth, multi-DB, IDE multi-server, etc.). Same `summarize`-then-`eval` workflow.

## API

For adapter formats (XState JSON), call `/api/v1/context/import` first to translate to CTXDSL, then call `/api/v1/context/eval` or `/api/v1/context/summarize`. For native CTXDSL files, send the content directly to the analysis endpoints.

```bash
# Translate XState then verify
CT=$(cat examples/agentic/support_pipeline.xstate.json)
CTXDSL=$(curl -s -X POST http://127.0.0.1:8080/api/v1/context/import \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$CT" '{format:"auto", content:$c}')" | jq -r '.ctxdsl')
curl -s -X POST http://127.0.0.1:8080/api/v1/context/eval \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$CTXDSL" \
        '{context: {name:"support_pipeline", content:$c},
          formula: "no_tool_over_budget", automaton: "support_pipeline_system"}')"
```

## UI

In `mununu-ui`, the editor recognizes `.xstate.json` and auto-routes through the import endpoint before summarizing. Native `.ctxdsl` files load directly. The summary panel shows the resolved automaton names (e.g. `support_pipeline_system`, `auth_system`, `handoff_system`) and all declared formulas.
