> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

# API Reference

Mununu exposes a REST API for programmatic access to context summarization, controller synthesis, graph generation, formula verification, predicate-abstraction CEGAR, AST extraction, assume/guarantee contracts, and HW/SW codesign. The server is built on [Axum](https://github.com/tokio-rs/axum) and listens on a configurable address (default `127.0.0.1:3000`).

> Source of truth: [`api::server::create_router`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/server.rs) — surface: API. (Route table — the canonical list of live endpoints.)

Start the server with:

```bash
mununu serve --bind 127.0.0.1:3000
```

All request and response bodies use `application/json`. CORS is open by default (`Access-Control-Allow-Origin: *`). The request body limit is 1 MiB and the request timeout is 30 s.

---

## Table of Contents

**Health**
- [GET /api/v1/health](#get-apiv1health)

**Context (CTXDSL) endpoints**
- [POST /api/v1/context/summarize](#post-apiv1contextsummarize)
- [POST /api/v1/context/synthesize](#post-apiv1contextsynthesize)
- [POST /api/v1/context/graphs](#post-apiv1contextgraphs)
- [POST /api/v1/context/verify](#post-apiv1contextverify)
- [POST /api/v1/context/import](#post-apiv1contextimport)
- [POST /api/v1/context/predicates](#post-apiv1contextpredicates)

**Verification framework (N-source)**
- [POST /api/v1/verify](#post-apiv1verify)
- [POST /api/v1/verify/memory-check](#post-apiv1verifymemory-check)

**CEGAR (predicate-abstraction refinement)**
- [POST /api/v1/btor2/cegar](#post-apiv1btor2cegar)
- [POST /api/v1/sv/cegar](#post-apiv1svcegar)

**AST extraction**
- [GET /api/v1/extraction/domains](#get-apiv1extractiondomains)
- [GET /api/v1/extraction/composition-modes](#get-apiv1extractioncomposition-modes)
- [POST /api/v1/extraction/propose-composition](#post-apiv1extractionpropose-composition)
- [POST /api/v1/extraction/extract](#post-apiv1extractionextract)
- [POST /api/v1/extraction/validate](#post-apiv1extractionvalidate)

**Contracts (assume/guarantee, black-box interfaces)**
- [POST /api/v1/contract/validate](#post-apiv1contractvalidate)
- [POST /api/v1/contract/discover](#post-apiv1contractdiscover)
- [POST /api/v1/contract/query](#post-apiv1contractquery)
- [POST /api/v1/contract/review](#post-apiv1contractreview)

**HW/SW codesign**
- [POST /api/v1/codesign/verify](#post-apiv1codesignverify)
- [POST /api/v1/codesign/reconcile-labels](#post-apiv1codesignreconcile-labels)
- [POST /api/v1/codesign/emit-chaotic-stub](#post-apiv1codesignemit-chaotic-stub)

**Templates**
- [GET /api/v1/templates](#get-apiv1templates)

**Reference**
- [Common Types](#common-types)
- [Error Responses](#error-responses)
- [Logging](#logging)

---

## GET /api/v1/health

Returns the server health status. Use this for readiness probes and connectivity checks.

> Source of truth: [`api::handlers::health_check`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Response

```json
{
  "status": "ok",
  "service": "mununu-api"
}
```

### Example

```bash
curl http://localhost:3000/api/v1/health
```

---

## POST /api/v1/context/summarize

Parse a CTXDSL context (with optional sidecar files) and return a summary of all automata, formulas, and declared controllers. Controllers are synthesized on the fly so the response includes realizability and size metrics.

> Source of truth: [`api::handlers::context_summarize_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

> **Adapter formats:** this endpoint accepts CTXDSL only. To summarize an external format (`.xstate.json`, `.sv`, `.tlsf`, `.aag`/`.aig`, `.pml`, `.espec.json`), first translate via [`POST /api/v1/context/import`](#post-apiv1contextimport) and pass the resulting `ctxdsl` field to this endpoint. The CLI command `mununu context summarize <file> --adapter <format>` does this two-step internally; clients of the HTTP API must do it explicitly. The same convention applies to `/context/verify` and `/context/synthesize`.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `context` | `FileContent` | Yes | Main CTXDSL file. |
| `sidecars` | `SidecarFile[]` | No | Additional sidecar CTXDSL files merged into the context. Defaults to `[]`. |
| `format` | `"json" \| "table"` | No | Output format. Defaults to `"json"`. |

**FileContent / SidecarFile**

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | File name (used for diagnostics). |
| `content` | `string` | Full CTXDSL source text. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `summary.context_name` | `string` | Name declared in the context. |
| `summary.automata` | `AutomatonSummary[]` | One entry per automaton and composition. |
| `summary.formulas_count` | `number` | Total formulas defined in the context. |
| `summary.controllers_count` | `number` | Total controller declarations. |
| `summary.controllers` | `ControllerSummary[]` | Per-controller realizability and size. |

**AutomatonSummary**

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Automaton identifier. |
| `states_count` | `number` | Number of states. |
| `transitions_count` | `number` | Number of transitions. |

**ControllerSummary**

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Controller identifier. |
| `source` | `string` | Source automaton name. |
| `formula` | `string` | Formula used for synthesis. |
| `realizable` | `boolean` | Whether a winning strategy exists. |
| `states_count` | `number` | Controller state count (0 if unrealizable). |
| `transitions_count` | `number` | Controller transition count (0 if unrealizable). |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/context/summarize \
  -H "Content-Type: application/json" \
  -d '{
    "context": {
      "name": "traffic.ctxdsl",
      "content": "context traffic {\n  automata {\n    automaton light {\n      states { state red initial; state green; }\n      transitions { transition red -> green on go; transition green -> red on stop; }\n    }\n  }\n}"
    },
    "sidecars": []
  }'
```

### Example Response

```json
{
  "success": true,
  "summary": {
    "context_name": "traffic",
    "automata": [
      { "name": "light", "states_count": 2, "transitions_count": 2 }
    ],
    "formulas_count": 0,
    "controllers_count": 0,
    "controllers": []
  }
}
```

---

## POST /api/v1/context/synthesize

Synthesize a controller for a given automaton and mu-calculus formula. The controller restricts the system's controllable transitions so that the formula is satisfied. When the specification is realizable, the response includes the controller serialized as CTXDSL source (and, optionally, in a native target format).

> Source of truth: [`api::handlers::context_synthesize_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `context` | `FileContent` | Yes | Main CTXDSL file. |
| `sidecars` | `SidecarFile[]` | No | Sidecar files. Defaults to `[]`. |
| `automaton` | `string` | Yes | Target automaton name. |
| `formula` | `string \| null` | One of `formula` / `template_ref` | Formula name defined in the context. Mutually exclusive with `template_ref`. |
| `template_ref` | `TemplateRef \| null` | One of `formula` / `template_ref` | Instantiate a [property template](Property-Templates) instead of selecting an existing formula. `{"template": "no_deadlock"}` or `{"template": "reachable", "args": {"TARGET": "Idle"}}`. Mutually exclusive with `formula`. |
| `options` | `SynthesisOptions` | No | Synthesis configuration. |

**SynthesisOptions**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `minimize` | `boolean` | `false` | Apply bisimulation minimization to the controller. |
| `diagnostics` | `DiagnosticsOptions` | `{}` | Control diagnostic output. |
| `extract_strategy` | `boolean` | `false` | **Legacy** — equivalent to `controller_mode: "functional"`. When `controller_mode` is set, that takes precedence. |
| `controller_mode` | `string \| null` | `null` (= `"projection"`, or `"functional"` if `extract_strategy=true`) | Controller extraction mode. One of `"projection"`, `"functional"`, `"permissive"`, `"signature-memory"`, `"product-game"`, `"parity-game"`. Case-insensitive; dashes/underscores interchangeable. Unknown values return `400 Bad Request`. See [Controller Modes](Controller-Modes.md) for the full reference. |
| `output_format` | `string \| null` | `null` | Native controller export format: `"xstate"` or `"systemverilog"`/`"sv"`. When set, the response includes a `controller_native` field. |

**DiagnosticsOptions**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `counterexample` | `boolean` | `false` | Compute a counterexample trace when unrealizable. |
| `counterstrategy` | `boolean` | `false` | Compute environment counterstrategy traces. |
| `deadlock_traces` | `boolean` | `false` | Include deadlock traces in diagnostics. |
| `max_counter_traces` | `number \| null` | `null` | Limit the number of counterstrategy traces. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `realizable` | `boolean` | Whether a winning controller exists. |
| `controller` | `FileContent \| null` | Synthesized controller as CTXDSL. `null` when unrealizable. |
| `controller_native` | `FileContent \| null` | Controller in the requested native format (`output_format`). Omitted unless requested. |
| `diagnostics` | `SynthesisDiagnostics` | Diagnostic information. |
| `counterstrategy` | `CounterstrategyResult \| null` | Environment counterstrategy graph for unrealizable cases. Automatically computed when synthesis fails. See the `CounterstrategyResult` table under [POST /api/v1/context/verify](#post-apiv1contextverify). |

**SynthesisDiagnostics**

| Field | Type | Description |
|-------|------|-------------|
| `messages` | `string[]` | Human-readable diagnostic messages. |
| `violating_initials` | `string[]` | Initial states that violate the formula. |
| `counterexample_trace` | `string[] \| null` | A trace witnessing a violation (if requested). |
| `counterstrategy_traces` | `string[][]` | Environment counterstrategy traces. |
| `deadlock_traces` | `string[][]` | Traces leading to deadlock states. |
| `lasso_traces` | `LassoTrace[]` | Lasso-format infinite counterexample traces (prefix + cycle). Present for liveness violations. |
| `minimization` | `MinimizationReport \| null` | Minimization statistics (if `minimize` was `true`). |
| `proof_obligations` | `ProofObligation[]` | Per-state proof obligations. |

**LassoTrace**

| Field | Type | Description |
|-------|------|-------------|
| `prefix` | `string[]` | State names forming the finite prefix. |
| `cycle` | `string[]` | State names forming the infinitely repeating cycle. |
| `prefix_labels` | `string[]` | Transition labels between consecutive prefix states. `prefix_labels[i]` is the label from `prefix[i]` to `prefix[i+1]` (or `cycle[0]` for the last). |
| `cycle_labels` | `string[]` | Transition labels between consecutive cycle states. The last element is the label from the last cycle state back to `cycle[0]`. |

**MinimizationReport**

| Field | Type | Description |
|-------|------|-------------|
| `removed_states` | `number` | States removed by minimization. |
| `removed_transitions` | `number` | Transitions removed. |
| `merged_states` | `string[]` | Names of states that were merged. |

**ProofObligation**

| Field | Type | Description |
|-------|------|-------------|
| `state` | `string` | State name. |
| `detail` | `string \| null` | Additional detail about the obligation. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/context/synthesize \
  -H "Content-Type: application/json" \
  -d '{
    "context": { "name": "arbiter.ctxdsl", "content": "... CTXDSL source ..." },
    "automaton": "arbiter",
    "formula": "mutual_exclusion",
    "options": {
      "minimize": true,
      "diagnostics": { "counterexample": true, "deadlock_traces": true, "max_counter_traces": 5 }
    }
  }'
```

### Example Response (Realizable)

```json
{
  "success": true,
  "realizable": true,
  "controller": {
    "name": "arbiter_controller.ctxdsl",
    "content": "// Synthesised controller derived from automaton 'arbiter' and formula 'mutual_exclusion'\ncontext arbiter_mutual_exclusion_controller {\n  ...\n}"
  },
  "diagnostics": {
    "messages": [],
    "violating_initials": [],
    "counterexample_trace": null,
    "counterstrategy_traces": [],
    "deadlock_traces": [],
    "minimization": { "removed_states": 3, "removed_transitions": 7, "merged_states": ["s1_s3", "s2_s4"] },
    "proof_obligations": []
  }
}
```

### Example Response (Unrealizable)

```json
{
  "success": true,
  "realizable": false,
  "controller": null,
  "diagnostics": {
    "messages": ["No winning strategy exists from initial state idle"],
    "violating_initials": ["idle"],
    "counterexample_trace": ["idle", "req1", "grant1", "req2"],
    "counterstrategy_traces": [],
    "deadlock_traces": [],
    "lasso_traces": [],
    "minimization": null,
    "proof_obligations": []
  },
  "counterstrategy": {
    "environment_winning_states": ["idle", "req1"],
    "graph_elements": [
      { "data": { "type": "node", "id": "cs_idle", "label": "idle", "vars": [], "actions": [] }, "classes": "env-winning start" },
      { "data": { "type": "edge", "id": "cs_e0", "source": "cs_idle", "target": "cs_req1", "label": "request", "action_type": "uncontrollable" } }
    ],
    "inverted_formula": "nu X. (!goal && <ctrl=environment> X)",
    "minimized": true
  }
}
```

---

## POST /api/v1/context/graphs

Generate Cytoscape-compatible graph elements for automata, compositions, and (optionally) synthesized controllers. Returns nodes and edges that can be rendered directly in a Cytoscape.js visualization layer.

> Source of truth: [`api::handlers::context_graphs_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `context` | `FileContent` | Yes | Main CTXDSL file. |
| `sidecars` | `SidecarFile[]` | No | Sidecar files. Defaults to `[]`. |
| `automaton` | `string \| null` | No | Filter to a single automaton. `null` returns all. |
| `graph_types` | `("dsl" \| "unrolled")[]` | No | Which graph representations to generate. Defaults to `["dsl"]`. |
| `include_controllers` | `boolean` | No | Include graphs for declared controllers. Defaults to `false`. |
| `minimize_controllers` | `boolean \| null` | No | Override per-controller minimize setting. `null` uses each controller's declared setting. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `context` | `ContextSummary` | Summary of the parsed context. |
| `graphs` | `GraphData[]` | One entry per automaton per graph type. |

**GraphData**

| Field | Type | Description |
|-------|------|-------------|
| `automaton` | `string` | Automaton name (or `<name>_controller` for controller graphs). |
| `graph_type` | `"dsl" \| "unrolled" \| "controller"` | Type of graph. |
| `elements` | `GraphElement[]` | Cytoscape-compatible elements. |
| `metadata` | `GraphMetadata` | Aggregate statistics. |

**GraphElement**

| Field | Type | Description |
|-------|------|-------------|
| `data` | `GraphElementData` | Node or edge data (discriminated by `data.type`). |
| `position` | `{ x: number, y: number } \| null` | Optional layout coordinates. |
| `classes` | `string \| null` | CSS class names for styling. |

**GraphElementData (Node)**

| Field | Type | Description |
|-------|------|-------------|
| `type` | `"node"` | Discriminator. |
| `id` | `string` | Unique node ID. |
| `label` | `string` | Display label. |
| `parent` | `string \| null` | Parent node ID for compound graphs. |
| `vars` | `string[]` | State variable annotations. |
| `actions` | `string[]` | Enabled actions. |
| `valuations` | `{ [name: string]: string } \| null` | Structured per-state variable valuations (e.g. `{is_red: "0", phase: "green"}`). Sourced from adapter side-channels (SV Kripke, BTOR2, extraction). Omitted when empty. |

**GraphElementData (Edge)**

| Field | Type | Description |
|-------|------|-------------|
| `type` | `"edge"` | Discriminator. |
| `id` | `string` | Unique edge ID. |
| `source` | `string` | Source node ID. |
| `target` | `string` | Target node ID. |
| `label` | `string \| null` | Transition label. |
| `action` | `string \| null` | Action name. |
| `action_type` | `string \| null` | `"controllable"` or `"uncontrollable"`. |
| `guard` | `string \| null` | Guard expression. |
| `effect` | `string \| null` | Effect expression. |
| `modality` | `string \| null` | KMTS transition modality: `"sharp"`, `"may_only"`, or `"must_hyper_only"`. Omitted (≡ `"sharp"`) on edges that don't come from a CLTS transition or for the default case. |

**GraphMetadata**

| Field | Type | Description |
|-------|------|-------------|
| `states_count` | `number` | Number of states in the graph. |
| `transitions_count` | `number` | Number of transitions. |
| `initial_states` | `string[]` | Names of initial states. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/context/graphs \
  -H "Content-Type: application/json" \
  -d '{
    "context": { "name": "traffic.ctxdsl", "content": "... CTXDSL source ..." },
    "graph_types": ["dsl", "unrolled"],
    "include_controllers": true,
    "minimize_controllers": true
  }'
```

### Example Response (abbreviated)

```json
{
  "success": true,
  "context": {
    "context_name": "traffic",
    "automata": [{ "name": "light", "states_count": 2, "transitions_count": 2 }],
    "formulas_count": 1,
    "controllers_count": 0,
    "controllers": []
  },
  "graphs": [
    {
      "automaton": "light",
      "graph_type": "dsl",
      "elements": [
        { "data": { "type": "node", "id": "light_red", "label": "red", "vars": [], "actions": ["go"] }, "position": null, "classes": "state initial" },
        { "data": { "type": "node", "id": "light_green", "label": "green", "vars": [], "actions": ["stop"] }, "position": null, "classes": "state" },
        { "data": { "type": "edge", "id": "light_e0", "source": "light_red", "target": "light_green", "label": "go", "action": "go", "action_type": "controllable" }, "position": null, "classes": null }
      ],
      "metadata": { "states_count": 2, "transitions_count": 2, "initial_states": ["red"] }
    }
  ]
}
```

---

## POST /api/v1/context/verify

Evaluate mu-calculus formulas over automata and report which initial states satisfy or violate each formula. When a formula is not satisfied and `counterstrategy` is requested, the response includes the environment's winning region and a Cytoscape graph of the counterstrategy automaton.

> Source of truth: [`api::handlers::context_verify_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `context` | `FileContent` | Yes | Main CTXDSL file. |
| `sidecars` | `SidecarFile[]` | No | Sidecar files. Defaults to `[]`. |
| `formula` | `string \| null` | No | Evaluate a specific formula by name. `null` evaluates all user-defined formulas. Mutually exclusive with `template_ref`. |
| `template_ref` | `TemplateRef \| null` | No | Instantiate a [property template](Property-Templates) instead of selecting an existing formula. `{"template": "no_deadlock"}` or `{"template": "reachable", "args": {"TARGET": "Idle"}}`. Mutually exclusive with `formula`. |
| `automaton` | `string \| null` | No | Target automaton. `null` uses each formula's declared targets. |
| `counterstrategy` | `boolean` | No | Compute counterstrategy for failed formulas. Defaults to `false`. |
| `minimize_counterstrategy` | `boolean` | No | Apply bisimulation minimization to counterstrategy. Defaults to `false`. |
| `hide` | `string[]` | No | Labels to hide (reclassify as internal) before evaluation. Defaults to `[]`. |
| `minimize` | `boolean` | No | Apply bisimulation minimization before evaluation. Defaults to `false`. |
| `stubs` | `SidecarFile[]` | No | Stub `.espec.json` content to compose as sidecars (interface automata). Defaults to `[]`. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `all_satisfied` | `boolean` | `true` if every formula is satisfied on every target. |
| `results` | `FormulaVerificationResult[]` | One entry per formula-automaton pair. |

**FormulaVerificationResult**

| Field | Type | Description |
|-------|------|-------------|
| `formula_name` | `string` | Formula identifier. |
| `automaton` | `string` | Automaton evaluated against. |
| `satisfied` | `boolean` | `true` if all initial states satisfy the formula. |
| `total_states` | `number` | Total states in the automaton. |
| `satisfying_states` | `number` | Number of states satisfying the formula. |
| `initial_states` | `string[]` | All initial state names. |
| `initial_satisfying` | `string[]` | Initial states that satisfy the formula. |
| `initial_violating` | `string[]` | Initial states that violate the formula. |
| `satisfying_state_names` | `string[]` | All satisfying state names (sorted). |
| `counterstrategy` | `CounterstrategyResult \| null` | Present when requested and formula is not satisfied. |

**CounterstrategyResult**

| Field | Type | Description |
|-------|------|-------------|
| `environment_winning_states` | `string[]` | States where the environment can force a violation. |
| `graph_elements` | `GraphElement[]` | Cytoscape graph of the counterstrategy. |
| `inverted_formula` | `string` | The negated formula used internally (for debugging). |
| `minimized` | `boolean` | Whether minimization was applied. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/context/verify \
  -H "Content-Type: application/json" \
  -d '{
    "context": { "name": "arbiter.ctxdsl", "content": "... CTXDSL source ..." },
    "formula": "mutual_exclusion",
    "automaton": "arbiter",
    "counterstrategy": true,
    "minimize_counterstrategy": false
  }'
```

### Example Response (Satisfied)

```json
{
  "success": true,
  "all_satisfied": true,
  "results": [
    {
      "formula_name": "mutual_exclusion",
      "automaton": "arbiter",
      "satisfied": true,
      "total_states": 6,
      "satisfying_states": 6,
      "initial_states": ["idle"],
      "initial_satisfying": ["idle"],
      "initial_violating": [],
      "satisfying_state_names": ["idle", "grant1", "grant2", "wait1", "wait2", "reset"]
    }
  ]
}
```

---

## POST /api/v1/context/import

Import an external format (XState, SystemVerilog, TLSF, AIGER, BTOR2, Promela, CrewAI, LangGraph, extraction) and translate it to CTXDSL.

> Source of truth: [`api::handlers::context_import_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `content` | `string` | Yes | Raw file content in the source format. |
| `format` | `string` | No | Format hint: `"auto"` (default), `"tlsf"`, `"aiger"`, `"btor2"` (or `"btor"`), `"promela"`, `"xstate"`, `"systemverilog"` (hand-written parser), `"sv-yosys"` (or `"yosys"`, Yosys-driven elaboration), `"extraction"`, `"crewai"`, `"langgraph"`. |
| `filename` | `string \| null` | No | Original filename (used for extension-based detection if format is `"auto"`). |
| `sidecar` | `string \| null` | No | Optional sidecar content (`.mununu.json` for SV, `.espec.json` for extraction). Drives abstraction/property configuration. |
| `additional_sources` | `FileContent[]` | No | Extra SV source files to compile alongside the primary input (Yosys path). Each entry: `{name, content}`. |
| `use_sv2v` | `boolean` | No | When `format == "sv-yosys"`, run the `sv2v` preprocessor before Yosys. Required for modern SV dialects. Mirrors the CLI `--preprocessor sv2v`. Defaults to `false`. |
| `predicates` | `PredicateSpecRequest[]` | No | Predicate set for the controllability-aware predicate-cube lift. Each entry: `{name, register, value}`. When non-empty (with `controllable_inputs`) and `format` produces BTOR2, the predicate-cube lift runs and a KMTS is returned. Mirrors `--predicate NAME:REG=VALUE`. |
| `controllable_inputs` | `string[]` | No | Names of BTOR2 input symbols the controller drives. Mirrors `--controllable-input`. Enables the controllability-aware lift when combined with `predicates`. |
| `sv_source_path` | `string \| null` | No | Filesystem path to the original SV source. With a `simulate_reset` sidecar block and a discoverable Verilator, runs a short reset simulation. Mirrors `--sv-source`. |
| `sidecar_path` | `string \| null` | No | Filesystem path to the sidecar JSON, used to resolve relative `vcd_traces` entries. Mirrors `--sidecar`. |

**PredicateSpecRequest**

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Human-readable predicate name (e.g. `"burst_zero"`). |
| `register` | `string` | BTOR2 register symbol the predicate is anchored on. |
| `value` | `number` | Integer value the predicate witnesses (`register == value`). |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | Whether the import succeeded. |
| `ctxdsl` | `string` | Translated CTXDSL content. |
| `source_format` | `string` | Detected source format name. |
| `warnings` | `string[]` | Translation warnings (unsupported constructs, neutral controllability, etc.). |
| `signal_count` | `number` | Number of signals/events in the source. |
| `state_count` | `number` | Number of states (for signal-state encoding). |
| `property_count` | `number` | Number of properties translated. |
| `state_valuations` | `object \| null` | State valuations for structured predicate matching, when available. |
| `transition_observations` | `object \| null` | Per-transition Mealy observations, keyed by automaton name, when the adapter emits them. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/context/import \
  -H "Content-Type: application/json" \
  -d '{
    "content": "{\"id\":\"light\",\"initial\":\"green\",\"states\":{\"green\":{\"on\":{\"TIMER\":\"yellow\"}},\"yellow\":{\"on\":{\"TIMER\":\"red\"}},\"red\":{\"on\":{\"TIMER\":\"green\"}}}}",
    "format": "xstate"
  }'
```

See [Adapter Formats](Adapter-Formats.md) for details on each supported format.

---

## POST /api/v1/context/predicates

List the predicate names declared per automaton in a parsed and realized context. Mirrors `mununu context predicates`. Useful for populating predicate pickers in clients before issuing a verify/synthesize request.

> Source of truth: [`api::handlers::context_predicates_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `context` | `FileContent` | Yes | Main CTXDSL file. |
| `sidecars` | `SidecarFile[]` | No | Sidecar files. Defaults to `[]`. |
| `automaton` | `string \| null` | No | Filter to a single automaton. `null` returns every automaton. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `predicates` | `{ [automaton: string]: string[] }` | Map from automaton name to its declared predicate names. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/context/predicates \
  -H "Content-Type: application/json" \
  -d '{ "context": { "name": "m.ctxdsl", "content": "... CTXDSL source ..." } }'
```

---

## POST /api/v1/verify

Run the general N-source verification framework against a `verify.toml` manifest. Mirrors `mununu verify` (CLI). See [Verify Project Flow](Verify-Project-Flow.md) for the conceptual model.

> Source of truth: [`api::handlers::verify_project_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

Supply exactly one of `config` (pre-parsed) or `config_toml` (raw verify.toml text); the handler 400s on both-set or neither-set.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `config` | `object \| null` | No | Pre-parsed `VerifyConfig` JSON. Mutually exclusive with `config_toml`. |
| `config_toml` | `string \| null` | No | Raw verify.toml text. Parsed server-side via `VerifyConfig::from_toml`. Mutually exclusive with `config`. Convenient for thin clients (UI wizard) that don't bundle a TOML parser. |
| `base_dir` | `string` | Yes | Directory the source paths in the config resolve against. Must exist on the server's filesystem. |
| `cluster_similarity_floor` | `number \| null` | No | R.4 clustered-COI Jaccard similarity floor for the BTOR2 (`sv-yosys`) route. Overrides any value in `config` / `config_toml`. `null` (default) → the recommended `0.5`. |

### Response Body

`VerifyReport` — see [`verify::report::VerifyReport`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/report.rs) for the full shape.

| Field | Type | Description |
|-------|------|-------------|
| `project` | `string` | Project name from `[project]`. |
| `sources` | `SourceSummary[]` | Per-source diagnostics (`id`, `adapter`, resolved automaton name). |
| `composition` | `CompositionInfo` | `semantics`, resolved composition `name`, resolved member names. |
| `property_verdicts` | `PropertyVerdict[]` | One verdict per `[[properties]]` entry. |

`PropertyVerdict` carries `name`, `formula_source` (`Inline` or `Template { id, args }`), the concrete `formula` text, the `over` target, `satisfied`, state counts, and the initial-state breakdown.

### Example

```bash
TOML=$(cat examples/verify/crewai_handoff/verify.toml)
BASE=$(realpath examples/verify/crewai_handoff)
curl -X POST http://localhost:3000/api/v1/verify \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg toml "$TOML" --arg base "$BASE" \
        '{config_toml: $toml, base_dir: $base}')"
```

---

## POST /api/v1/verify/memory-check

Analyze a `verify.toml` config's memory posture and return advisory warnings (over-approximation risks, unbounded counters, large enumeration domains). Mirrors `mununu verify memory-check`. The analysis is **pure** (it inspects only the parsed config), so no `base_dir` is required. The handler is **advisory** — warnings appear in the body but never surface as a 4xx; callers decide whether to gate on them.

> Source of truth: [`api::handlers::memory_check_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

Supply exactly one of `config` or `config_toml`.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `config` | `object \| null` | No | Pre-parsed `VerifyConfig` JSON. Mutually exclusive with `config_toml`. |
| `config_toml` | `string \| null` | No | Raw verify.toml text. Mutually exclusive with `config`. |

### Response Body

`MemoryCheckReport` — see [`verify::memory_check::MemoryCheckReport`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/memory_check.rs) for the full shape (per-source posture entries plus aggregate advisory warnings).

### Example

```bash
curl -X POST http://localhost:3000/api/v1/verify/memory-check \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg toml "$(cat verify.toml)" '{config_toml: $toml}')"
```

---

## POST /api/v1/btor2/cegar

Run the CEGAR predicate-abstraction-refinement loop over a BTOR2 design and return the per-iteration refinement trace. Mirrors `mununu btor2 cegar`. See [Predicate-Cube CEGAR](Predicate-Cube-CEGAR.md) for the algorithm and the 3-valued (Kleene) verdict semantics.

> Source of truth: [`api::handlers::btor2_cegar_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `content` | `string` | Yes | BTOR2 source content. |
| `formula` | `string` | Yes | μ-calculus formula evaluated over the lifted KMTS. |
| `predicates` | `PredicateSpecRequest[]` | Yes | Initial predicate set (bootstraps the `2^|P|` cube space). At least one entry required. Each: `{name, register, value}`. |
| `controllable_inputs` | `string[]` | No | R.6.6 controllability split — controller-driven input symbols. Mirrors `--controllable-input`. Defaults to `[]`. |
| `predicate_source` | `string \| null` | No | Predicate-discovery source: `"wp"` (default) or `"craig"`. |
| `max_iterations` | `number \| null` | No | Max CEGAR iterations. Defaults to `16`. |
| `must_edge_inference` | `string \| null` | No | Must-edge inference policy (default `"off"`): `"sampling-confluence"`, `"smt-per-target"`, `"smt-per-target-standard"`, `"smt-hyper-must"`. |
| `may_edge_inference` | `string \| null` | No | May-edge inference policy (default `"off"`): `"smt-all-pairs"`. |
| `config_values` | `string[]` | No | R-S8 symbolic-init values, one entry per register as `"REG=v1,v2,..."`. Seeds the predicate-cube initial states. Defaults to `[]`. |
| `emit_ctxdsl` | `boolean` | No | When `true`, the response `ctxdsl` field carries the final refined cube model + checked formula as a self-contained CTXDSL document. Mirrors `--emit-ctxdsl`. Defaults to `false`. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `iterations` | `CegarIterationView[]` | Per-iteration refinement records (iteration 0 = initial evaluation). |
| `final_predicates` | `PredicateView[]` | Predicate set at termination (initial + every added predicate). |
| `terminated_with` | `string` | Why the loop stopped: `"converged"`, `"bounded-iterations-reached"`, or `"predicate-source-exhausted"`. |
| `verdict` | `CegarVerdictSummary` | Cell-count summary of the final 3-valued verdict. |
| `lazy_lift_pending` | `boolean` | `true` when the eager `predicate_cube_lift` was used. |
| `approximant_reuse_enabled` | `boolean` | Whether prior-iteration approximants were threaded forward. |
| `warnings` | `string[]` | Soundness / advisory warnings produced during the run. |
| `ctxdsl` | `string \| null` | Final refined cube model + formula as CTXDSL, present only when `emit_ctxdsl: true`. |

**CegarIterationView**

| Field | Type | Description |
|-------|------|-------------|
| `iteration` | `number` | Iteration index. |
| `predicate_count` | `number` | Predicate-set size at the start of this iteration. |
| `had_failure_subgame` | `boolean` | `true` iff this iteration's verdict carried `KleeneBot` cells (drove a refinement). |
| `predicates_added` | `PredicateView[]` | Predicates the source added in response to this iteration. |
| `game_position_evaluations` | `number` | Proxy counter for game-position evaluations (approximant-reuse diagnostics). |
| `verdict` | `CegarVerdictSummary` | Cell-count summary of this iteration's 3-valued verdict. |

**CegarVerdictSummary**

| Field | Type | Description |
|-------|------|-------------|
| `true_cells` | `number` | `KleeneT` (definitely-true) cells. |
| `false_cells` | `number` | `KleeneF` (definitely-false) cells. |
| `unknown_cells` | `number` | `KleeneBot` (unknown — needs refinement) cells. |

**PredicateView** — `{ name: string, register: string, value: number }`.

### Example

```bash
curl -X POST http://localhost:3000/api/v1/btor2/cegar \
  -H "Content-Type: application/json" \
  -d '{
    "content": "... BTOR2 source ...",
    "formula": "nu X. ([] X && mu Y. (done || <> Y))",
    "predicates": [ { "name": "burst_zero", "register": "burst_cnt", "value": 0 } ],
    "controllable_inputs": ["start"],
    "max_iterations": 8
  }'
```

---

## POST /api/v1/sv/cegar

SV-direct CEGAR (cegar-extraction Stage 2): lift a SystemVerilog design to a single flattened BTOR2 (sv2v + Yosys) in one call, then run the same predicate-abstraction refinement loop as [`/api/v1/btor2/cegar`](#post-apiv1btor2cegar) and return the same response. Mirrors `mununu sv cegar`. Lets an SV workflow run CEGAR without a manual emit-BTOR2-per-module step.

> Source of truth: [`api::handlers::sv_cegar_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

The CEGAR fields (`formula`, `predicates`, `controllable_inputs`, `predicate_source`, `max_iterations`, `must_edge_inference`, `may_edge_inference`, `config_values`, `emit_ctxdsl`) are identical to [`/api/v1/btor2/cegar`](#post-apiv1btor2cegar). Only the source half differs:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `source` | `string` | Yes | SystemVerilog primary source content. |
| `additional_sources` | `FileContent[]` | No | Additional SV source files (multi-file designs / packages / include targets). Defaults to `[]`. |
| `top` | `string \| null` | No | Top module name. Recommended for multi-module designs; `null` lets Yosys auto-detect. |
| `use_sv2v` | `boolean` | No | Run sv2v before Yosys (required for modern SV). Mirrors `--preprocess-sv2v`. Defaults to `false`. |
| `setundef_anyseq` | `boolean` | No | Yosys `setundef -anyseq` (per-cycle havoc on undefined nets). Defaults to `false`. |
| `setundef_anyconst` | `boolean` | No | Yosys `setundef -anyconst` (one nondeterministic constant per undefined bit — the Caliptra CWE-1245 power-up policy). Defaults to `false`. |
| `formula` | `string` | Yes | μ-calculus formula (see `/btor2/cegar`). |
| `predicates` | `PredicateSpecRequest[]` | Yes | Initial predicate set (see `/btor2/cegar`). |
| _CEGAR fields…_ | | No | `controllable_inputs`, `predicate_source`, `max_iterations`, `must_edge_inference`, `may_edge_inference`, `config_values`, `emit_ctxdsl` — identical to `/btor2/cegar`. |

### Response Body

Identical to [`/api/v1/btor2/cegar`](#post-apiv1btor2cegar) (`Btor2CegarResponse`).

### Example

```bash
curl -X POST http://localhost:3000/api/v1/sv/cegar \
  -H "Content-Type: application/json" \
  -d '{
    "source": "module counter(...); ... endmodule",
    "top": "counter",
    "use_sv2v": true,
    "formula": "nu X. ([] X && mu Y. (done || <> Y))",
    "predicates": [ { "name": "burst_zero", "register": "burst_cnt", "value": 0 } ]
  }'
```

---

## GET /api/v1/extraction/domains

List the available domain profiles (language + description) for AST extraction. Mirrors `mununu extraction domains`.

> Source of truth: [`api::handlers::extraction_domains_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `profiles` | `DomainProfileInfo[]` | Available domain profiles. |

**DomainProfileInfo** — `{ name: string, language: string, description: string }`.

### Example

```bash
curl http://localhost:3000/api/v1/extraction/domains
```

---

## GET /api/v1/extraction/composition-modes

List the supported composition modes (synchronous / asynchronous) that the extraction config's `composition.type` accepts.

> Source of truth: [`api::handlers::extraction_composition_modes_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `modes` | `CompositionModeInfo[]` | Supported composition modes. |

**CompositionModeInfo** — `{ name: string, description: string }`.

### Example

```bash
curl http://localhost:3000/api/v1/extraction/composition-modes
```

---

## POST /api/v1/extraction/propose-composition

Scan source code for concurrency idioms and propose `composition.instances[]` / `shared[]` blocks for an extraction config. Output is **suggestion-grade** — the user reviews each finding before promoting it into the config. Mirrors `mununu extraction propose-composition`. An empty `findings` list is the common case, not an error.

> Source of truth: [`api::handlers::extraction_propose_composition_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API. (Requires the `ast-extract` feature.)

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `source` | `string` | Yes | Source content to scan. |
| `language` | `string \| null` | No | Source language: `"typescript"`, `"python"`, or `"rust"`. There is no filename to infer from here, so callers should specify it. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `findings` | `DetectedConcurrency[]` | Detected concurrency findings in source order. See [`concurrency_detect::DetectedConcurrency`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/extraction/ast_extract/concurrency_detect.rs) for the full shape (`kind`, source span, `suggested_instance_names`, `suggested_class_hint`). |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/extraction/propose-composition \
  -H "Content-Type: application/json" \
  -d '{ "source": "... TypeScript source ...", "language": "typescript" }'
```

---

## POST /api/v1/extraction/extract

Run AST-based extraction from source code (TypeScript / Python / Rust) and produce an `.espec.json` extraction spec. Mirrors `mununu extraction extract`.

> Source of truth: [`api::handlers::extraction_extract_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API. (Requires the `ast-extract` feature.)

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `config` | `string` | Yes | Extraction config content (`.extract.json`). |
| `source` | `string` | Yes | Source code content. |
| `language` | `string \| null` | No | Source language (`typescript`, `python`, `rust`). Auto-detected if omitted. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `espec` | `string` | Generated `.espec.json` content. |
| `warnings` | `string[]` | Extraction warnings. |
| `automata` | `ExtractionAutomatonInfo[]` | Extracted automata. |

**ExtractionAutomatonInfo** — `{ id: string, state_count: number, transition_count: number }`.

### Example

```bash
curl -X POST http://localhost:3000/api/v1/extraction/extract \
  -H "Content-Type: application/json" \
  -d '{ "config": "... .extract.json ...", "source": "... source ...", "language": "typescript" }'
```

See [Compositional Extraction Tutorial](Compositional-Extraction-Tutorial.md) for an end-to-end walkthrough.

---

## POST /api/v1/extraction/validate

Validate an extraction spec against its source code: detect drifted/mismatched anchors and uncovered accesses. Mirrors `mununu extraction validate`.

> Source of truth: [`api::handlers::extraction_validate_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `spec` | `string` | Yes | Extraction spec (`.espec.json`) content. |
| `source` | `string` | Yes | Source code content to validate against. |
| `drift_window` | `number` | No | Line window for fuzzy anchor matching. Defaults to `5`. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | `true` on success. |
| `summary` | `ValidationSummaryApi` | Aggregate counts: `total`, `exact`, `drifted`, `mismatch`, `error`, `uncovered_accesses`. |
| `anchors` | `AnchorResultApi[]` | Per-anchor results: `id`, `section`, `status`, `line`, `found_line`, `message`. |
| `uncovered` | `UncoveredAccessApi[]` | Accesses with no covering anchor: `line`, `field`, `content`. |
| `commit_match` | `boolean \| null` | Whether the spec's recorded commit matches the source, when determinable. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/extraction/validate \
  -H "Content-Type: application/json" \
  -d '{ "spec": "... .espec.json ...", "source": "... source ...", "drift_window": 5 }'
```

---

## POST /api/v1/contract/validate

Validate an assume/guarantee contract set's discharge graph (SCC analysis). Mirrors `mununu contract validate`. The request body **is** a `ContractSet` (no wrapper); the response **is** a `DischargeVerdict`.

> Source of truth: [`api::handlers::contract_validate_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

A [`contract::ContractSet`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/contract/mod.rs) JSON value — clauses (assume/guarantee) plus the discharge edges between them.

### Response Body

A [`contract::discharge::DischargeVerdict`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/contract/discharge.rs) — whether the discharge graph is sound, with any offending cycles / undischarged obligations.

### Example

```bash
curl -X POST http://localhost:3000/api/v1/contract/validate \
  -H "Content-Type: application/json" \
  -d '{ "clauses": [ ... ], "edges": [ ... ] }'
```

---

## POST /api/v1/contract/discover

Run phase-1 contract discovery on a black-box interface description: classify labels (controllable / uncontrollable), detect fairness gaps, and resolve `@mununu_interface contract://` corpus references. Mirrors `mununu contract discover`. The server still emits structured `tracing::warn!` diagnostics; the response carries the full `Phase1Output` for the UI.

> Source of truth: [`api::handlers::contract_discover_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `interface` | `BlackBoxInterface` | Yes | Black-box interface description. See [`contract::discover::BlackBoxInterface`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/contract/discover.rs). |
| `force_controllable` | `string[]` | No | Labels to force-classify as controllable. Defaults to `[]`. |
| `force_uncontrollable` | `string[]` | No | Labels to force-classify as uncontrollable. Defaults to `[]`. |
| `emit_fairness_gap` | `boolean` | No | Emit fairness-gap markers. Defaults to `false`. |
| `corpus` | `string \| null` | No | Filesystem path to a contract corpus root used to resolve `contract://` URIs. Mirrors `--corpus`. |

### Response Body

A [`contract::discover::Phase1Output`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/contract/discover.rs) — classified labels, fairness-gap markers, and corpus resolutions.

---

## POST /api/v1/contract/query

Query the contract corpus (Document D task D2) by `<domain>/<name>` plus parameters, and return the ranked candidate list. Mirrors `mununu contract query`.

> Source of truth: [`api::handlers::contract_query_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | `string` | Yes | `<domain>/<name>` identifier, e.g. `"rtl_protocol/axi4_slave"`. A malformed id returns `400`. |
| `corpus` | `string` | Yes | Filesystem path of the corpus root the server loads. A non-existent path is treated as an empty corpus. |
| `parameters` | `{ [key: string]: any }` | No | Parameters to match against (numbers, strings, bools). Defaults to `{}`. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `candidates` | `ContractEntry[]` | Ranked matching corpus entries. See [`corpus::ContractEntry`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/corpus/mod.rs). |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/contract/query \
  -H "Content-Type: application/json" \
  -d '{ "id": "rtl_protocol/axi4_slave", "corpus": "/srv/corpus", "parameters": { "data_width": 32 } }'
```

---

## POST /api/v1/contract/review

HITL stage-4 review surface (Document A §A7 / Document D §D.8). Wraps phase-1/phase-2 discovery and adds a flat list of proposed clauses extracted from `@mununu_assume` / `@mununu_guarantee` annotations and resolved corpus references. Mirrors `mununu contract review`. The approve/edit/reject UX lives in the CLI / UI surfaces.

> Source of truth: [`api::handlers::contract_review_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

Same shape as [`/api/v1/contract/discover`](#post-apiv1contractdiscover): `interface`, `force_controllable`, `force_uncontrollable`, `emit_fairness_gap`, `corpus`.

### Response Body

A [`contract::review::ReviewPackage`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/contract/review.rs) — the phase-1 output plus the proposed-clause list for the review UI.

---

## POST /api/v1/codesign/verify

HW/SW codesign verification (Document C task C4). Compose firmware CTXDSL with a register-map sidecar, splice the coupling fragment, realize the composed context, and evaluate a named formula. Returns the verdict plus the composed CTXDSL so the UI can render both. Mirrors `mununu codesign verify`.

> Source of truth: [`api::handlers::codesign_verify_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `register_map` | `RegisterMap` | Yes | Register-map sidecar as a parsed JSON value. Same shape the CLI loads from `register_map.json`. See [`codesign::register_map::RegisterMap`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/codesign/register_map.rs). |
| `firmware_ctxdsl` | `string` | Yes | Firmware CTXDSL document text. |
| `formula` | `string` | Yes | Formula name to evaluate in the composed context. |
| `automaton` | `string \| null` | No | Composition / automaton to evaluate over. Defaults to the codesign composition `<PERIPHERAL>System`. |
| `peripheral_automaton` | `string \| null` | No | Override for the peripheral automaton name. |
| `composition_name` | `string \| null` | No | Override for the composition name. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `satisfied` | `boolean` | Whether every initial state satisfies the formula. |
| `total_states` | `number` | States in the composed automaton/composition. |
| `satisfying_states` | `number` | States satisfying the formula. |
| `initial_states` | `string[]` | Initial state names. |
| `initial_satisfying` | `string[]` | Subset of `initial_states` satisfying the formula. |
| `composition` | `CodesignCompositionInfo` | `{ peripheral_automaton, composition_name, firmware_members, automaton }`. |
| `composed_ctxdsl` | `string` | The composed CTXDSL the verifier ran against. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/codesign/verify \
  -H "Content-Type: application/json" \
  -d '{
    "register_map": { ... },
    "firmware_ctxdsl": "context fw { ... }",
    "formula": "no_enable_mid_transaction"
  }'
```

---

## POST /api/v1/codesign/reconcile-labels

HW/SW codesign label-alphabet reconciliation (Document C §C.5 hard gate against silent over-approximation). Refuses to compose `firmware ‖ peripheral` when the two extractions disagree on the rendezvous-label alphabet. Mirrors `mununu codesign reconcile-labels`. **Always returns 200 OK**; the `mismatch` field distinguishes the outcome.

> Source of truth: [`api::handlers::codesign_reconcile_labels_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `firmware_labels` | `string[]` | Yes | Firmware-side rendezvous labels (the C extraction's alphabet). |
| `peripheral_labels` | `string[]` | Yes | Peripheral-side rendezvous labels (the SV extraction / register-map alphabet). |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `shared` | `string[]` | Shared canonical alphabet (sorted) when the alphabets agree; empty on mismatch. |
| `mismatch` | `ReconcileMismatch \| null` | `null` when the alphabets reconcile; otherwise `{ firmware_only, peripheral_only }`. See [`codesign::reconcile::ReconcileMismatch`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/codesign/reconcile.rs). |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/codesign/reconcile-labels \
  -H "Content-Type: application/json" \
  -d '{ "firmware_labels": ["reg_write_ctrl", "irq_ack"], "peripheral_labels": ["reg_write_ctrl", "irq_raise"] }'
```

---

## POST /api/v1/codesign/emit-chaotic-stub

Emit a standalone chaotic-stub CTXDSL document from a register-map sidecar. The result has its own `context { … }` wrapper, ready to drop into a `verify.toml` as a `ctxdsl` source. Mirrors `mununu codesign emit-chaotic-stub`.

> Source of truth: [`api::handlers::codesign_emit_chaotic_stub_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Request Body

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `register_map` | `RegisterMap` | Yes | Parsed register-map JSON sidecar. |
| `peripheral_automaton` | `string \| null` | No | Override for the peripheral automaton name. Defaults to the uppercased peripheral name; the context-block name is always `<AutomatonName>ChaoticStub`. |
| `strict` | `boolean` | No | When `true`, refuse to emit and return `400` if the register-map validator reports any issue. Defaults to `false`. |

### Response Body

| Field | Type | Description |
|-------|------|-------------|
| `ctxdsl` | `string` | The standalone chaotic-stub CTXDSL document. |
| `warnings` | `string[]` | Validation warnings from the register-map validator. Empty when the sidecar is well-formed. |

### Example

```bash
curl -X POST http://localhost:3000/api/v1/codesign/emit-chaotic-stub \
  -H "Content-Type: application/json" \
  -d '{ "register_map": { ... }, "strict": false }'
```

---

## GET /api/v1/templates

List available property templates. Templates provide parameterized mu-calculus formula patterns that can be used in `template_ref` fields of verify and synthesize requests.

> Source of truth: [`api::handlers::templates_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

### Query Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `domain` | `string` | No | Filter by domain: `rtl`, `agentic`, `software`, `synthesis`, `universal`. |

### Response Body

Without `domain` filter, returns a `TemplateCatalog`:

```json
{
  "version": "1.0",
  "templates": [
    {
      "id": "no_deadlock",
      "display_name": "No Deadlock",
      "description": "Every reachable state has at least one enabled transition",
      "kind": "safety",
      "role": "guarantee",
      "domains": ["universal"],
      "params": [],
      "formula_pattern": "nu X. (<> true && [] X)",
      "domain_hints": {},
      "tags": ["deadlock", "softlock", "safety"]
    }
  ]
}
```

With `domain` filter, returns a filtered array of `PropertyTemplate` objects.

### Example

```bash
curl http://localhost:3000/api/v1/templates
curl http://localhost:3000/api/v1/templates?domain=rtl
```

See [Property Templates](Property-Templates) for the full catalog and usage guide.

---

## Common Types

### FileContent

Used for the main context, sidecars, and synthesized controller output.

```json
{
  "name": "example.ctxdsl",
  "content": "context example { ... }"
}
```

### SidecarFile

Structurally identical to `FileContent`. Sidecars are merged into the main context during parsing, allowing modular specifications.

```json
{
  "name": "properties.ctxdsl",
  "content": "context example_props { mu_formulas { ... } }"
}
```

### PredicateSpecRequest

A register-value equality predicate, shared by `/context/import`, `/btor2/cegar`, and `/sv/cegar`.

```json
{ "name": "burst_zero", "register": "burst_cnt", "value": 0 }
```

---

## Error Responses

All endpoints return errors in a consistent format:

```json
{
  "error": {
    "code": "BAD_REQUEST",
    "message": "Failed to parse context: unexpected token at line 5",
    "details": "unexpected token 'xyz' at line 5, column 12"
  }
}
```

| HTTP Status | Code | Meaning |
|-------------|------|---------|
| 400 | `BAD_REQUEST` | Invalid input: parse errors, unknown formula/automaton names, both/neither of mutually-exclusive fields, malformed identifiers. |
| 408 | `REQUEST_TIMEOUT` | The request exceeded the server's 30 s timeout. |
| 500 | `INTERNAL_ERROR` | Server-side failure during realization, synthesis, or evaluation. |

> Note: `/codesign/reconcile-labels` and `/verify/memory-check` are **advisory** — they return 200 OK with the warning/mismatch in the body rather than a 4xx.

---

## Logging

Set the `RUST_LOG` environment variable to control server log verbosity:

```bash
RUST_LOG=mununu=info mununu serve
```

The server logs timing breakdowns for each request (parse, realize, evaluate durations in milliseconds).
