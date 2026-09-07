# Compositional Extraction

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

Compositional extraction lets you generate **multi-automaton models** from source code in one pass — for concurrency / race-condition modeling where N instances of a class contend for a shared resource. Pre-Phase-A this work was done by hand, writing the espec's `composition` block manually after looking at source. With compositional extraction, the user declares the topology in the extract config and the extractor stitches the per-instance automata together with correct label rewriting.

The complementary [Composition](Composition.md) page covers synchronous / asynchronous composition semantics in depth. This page focuses on the extraction-time mechanics. For a hands-on, copy-pasteable walkthrough with a working public fixture, see [Compositional-Extraction-Tutorial](Compositional-Extraction-Tutorial.md).

## When to use it

Use compositional extraction whenever the property you want to verify spans **multiple component instances** rather than a single class. The canonical patterns:

- N workers competing for a shared file or database
- A primary + N replicas with one of them holding a leader role
- A request handler + N inflight contexts (one per concurrent request)
- A scheduler + N tasks

If your property is single-class (e.g., "this server's lifecycle never reaches state X"), the existing single-class extraction (one entry in `targets[]`, no `composition.instances`) is what you want.

## Configuration shape

Add `instances` and (optionally) `shared` to the existing `composition` block in your `.extract.json`:

```json
{
  "$schema": "extraction_config_v1",
  "domain": "mcp_server",
  "language": "typescript",
  "source": { "file": "src/memory/index.ts" },

  "targets": [
    {
      "class": "KnowledgeGraphManager",
      "state_fields": ["state"],
      "methods": { "include": ["load", "save"] }
    }
  ],

  "composition": {
    "type": "asynchronous",
    "name": "memory_write_race",
    "instances": [
      { "of": "KnowledgeGraphManager", "as": "worker_a" },
      { "of": "KnowledgeGraphManager", "as": "worker_b" }
    ],
    "shared": ["ev_save"]
  }
}
```

| Field | Required | Meaning |
|---|---|---|
| `type` | yes | `"synchronous"` or `"asynchronous"`. See [Composition](Composition.md) for the full semantics. |
| `name` | yes | The composition's CTXDSL identifier. |
| `instances` | new (Phase A) | Each entry has `of` (class name, must match a `target.class`) and `as` (instance name; becomes the automaton id and per-instance prefix). |
| `shared` | new (Phase A) | Labels that synchronize across instances. Empty / omitted means full async (zero alphabet intersection). |

## Label rewriting algorithm

The rewriting is deterministic and three-rule:

1. For each instance `<name>` of class `<C>`, run the existing single-class extraction on `<C>`. The resulting automaton's id becomes `<name>`.
2. Every label `L` not in `composition.shared[]` becomes `<name>__<L>`.
3. Labels in `composition.shared[]` are kept verbatim across all instances.

By construction, the union of all instances' label sets has the shared labels as the alphabet intersection. The composition engine ([crates/mununu-core/src/composition/mod.rs](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/composition/mod.rs)) uses that intersection to enforce joint firing — which is exactly what the user wants for synchronization.

**Why prefix-by-default:** if the default were "no rewriting," all labels would intersect by name and accidentally synchronize. Prefix-by-default makes the safe choice (independence / no synchronization) the no-config behavior — users opt in to synchronization by listing labels.

## Worked example: MCP-005 file race

The `mcp-server-memory` package has a `KnowledgeGraphManager` class that reads → modifies → writes a JSON file. With two concurrent processes both calling `manager.save()`, a race produces a clobbered file (one process's modification overwrites the other's).

Single-class extraction would give one automaton — useless for the race. Compositional extraction with the config above produces:

| Instance | Labels |
|---|---|
| `worker_a` | `worker_a__ev_load`, `ev_save` (shared) |
| `worker_b` | `worker_b__ev_load`, `ev_save` (shared) |

The composition's alphabet intersection is `{ev_save}`. The async-composition engine interleaves the prefixed `*__ev_load` labels (worker-internal load operations) and forces both workers' `ev_save` to fire jointly. This produces the canonical race topology where:

- Either worker can independently `load` (becomes "has the v0 baseline")
- Both workers must `save` jointly (the synchronization point)
- A trace where both workers load v0 → both attempt save jointly → one wins, the other's save clobbers — is reachable

Verifying the race property:

```bash
mununu-extract ast manager_compose.extract.json \
  --source src/memory/index.ts \
  --output composed.espec.json

mununu context eval composed.espec.json \
  --formula no_clobber --automaton memory_write_race
# Verdict: 0/1 initials  (the property fails — race is reachable)

mununu context eval composed.espec.json \
  --formula clobber_reachable --automaton memory_write_race
# Verdict: 1/1 initials  (the corrupt state is reachable from initial — non-vacuous witness)
```

## Important notes

- **The label prefix from the domain profile applies first.** The `mcp_server` profile prefixes events with `ev_`, so the user lists `ev_save` in `composition.shared`, not `save`. Label-rewriting compares against the post-prefix name.
- **`noop` is never shared.** The state-space engine adds noop self-loops to keep states reachable. If they synchronized across instances, no instance could progress independently. The rewriter excludes `noop` from sharing regardless of the user's list.
- **Unknown class is a hard error.** If `instance.of` doesn't match any `target.class`, extraction aborts with a clear error message rather than silently producing an empty automaton.
- **The `members` field is auto-populated.** When using `instances`, the resulting espec's `composition.members` is filled from the resolved instance names. Don't write it manually.

## Concurrency property templates

Five concurrency-specific property templates ship in the agentic / universal domains. List them with:

```bash
mununu templates --domain agentic
```

> **Template freshness:** the registry that powers `mununu templates`, `GET /api/v1/templates`, and the UI template picker loads `builtin_templates.json` at **compile time** via Rust's `include_str!`. New templates added to that JSON file only appear in the binary's catalog after `cargo build`. If `mununu templates` is missing a template you just added, rebuild first: `cargo build --release -p mununu-cli`. The same caveat applies to the running API server (restart it) and to any UI build that bundles a stale binary.

| Template | Pattern | Use |
|---|---|---|
| `no_clobber` | `nu X. (!RESOURCE_CORRUPT && [] X)` | Safety: the shared resource never enters a corrupt state. |
| `clobber_reachable` | `mu X. (RESOURCE_CORRUPT \|\| <> X)` | Liveness witness for the corrupt state. **Always ship alongside `no_clobber`** to confirm the safety verdict isn't trivially satisfied by an empty model. |
| `mutual_exclusion_3` | 3-way pairwise exclusion | Compositions with three contending instances (e.g., 3-way leader election). The existing `mutual_exclusion` covers the 2-way case. |
| `bounded_handoff` | `nu X. ((!HANDOFF_TRIGGERED \|\| mu Y. (HANDOFF_COMPLETE \|\| <> Y)) && [] X)` | Once handoff is triggered, completion is reachable in finite steps. Maps to MCP-001's parallel-interrupt resume pattern. |
| `no_lost_update` | `nu X. ((!WRITE_STARTED \|\| WRITE_VISIBLE) && [] X)` | Every started write becomes externally visible. Direct match for MCP-002's contamination patterns. |

In an espec, reference a template by ID + parameters instead of writing the formula:

```json
"properties": [
  {
    "id": "no_clobber_safety",
    "template_ref": {
      "template": "no_clobber",
      "args": { "RESOURCE_CORRUPT": "F_clobbered" }
    },
    "over": "FileVar"
  }
]
```

## CLI / API quick reference

```bash
# CLI: extract a compositional config (no new flags — schema is config-driven)
mununu-extract ast config.extract.json --source src.ts --output out.espec.json

# CLI: list composition modes with semantics
mununu-extract ast --list-composition-modes /dev/null --source /dev/null

# API: extract via HTTP
curl -X POST http://localhost:8080/api/v1/extraction/extract \
  -H "Content-Type: application/json" \
  -d @request.json

# API: list composition modes
curl http://localhost:8080/api/v1/extraction/composition-modes
```

The web UI compositional extraction step is reachable via the `compose` step in the software-extraction workflow stepper, including the Phase B `Suggest from source` button described below.

## Phase B — `Suggest from source` (auto-detection pre-pass)

**Status:** B1 (syntactic) shipped. B2-B5 deferred (see architecture doc).

When you load a source file with a known extension (`.py` / `.ts` / `.tsx` / `.rs` / `.gd`) the compose step shows a **Suggest from source (Phase B)** button alongside the template starter. Clicking it scans the source for known concurrency idioms and lists each finding as a card with an **Apply** button:

| Detector | Fires on |
|----------|----------|
| `python_asyncio_gather` | `asyncio.gather(a(), b(), …)` — including the import-aliased / module-aliased forms |
| `typescript_promise_all` | `Promise.all([…])` and `Promise.allSettled([…])` |

Each finding tells you the source line, the number of parallel branches (when statically known), and a list of suggested instance names (`task_0`, `task_1`, …). Applying a finding writes a starting-point JSON config into the editor. **Three things you must do after applying:**

1. **Set `shared[]`.** The auto-generated config has an empty `shared` array — no synchronization. For race-detection / lost-update properties you almost always want at least one shared label (the `ev_*` event the contended resource exposes). The Phase B detector cannot infer this; it's a domain decision.
2. **Rename instances.** `task_0` / `task_1` are placeholders. Use names that reflect the real role (`worker_a`, `worker_b`, `producer`, `consumer`, …).
3. **Set `composition.name`.** The auto-generated `auto_<detector>_l<line>` is descriptive but ugly; rename to a domain term (`memory_write_race`, `parallel_handoff`, …).

### CLI / API equivalents

```bash
# CLI: scan source, print findings as JSON, exit
mununu-extract ast composition.extract.json \
  --source mcp_server.py \
  --language python \
  --propose-composition

# API: same, over HTTP
curl -X POST http://localhost:8080/api/v1/extraction/propose-composition \
  -H "Content-Type: application/json" \
  -d '{"source": "import asyncio\nawait asyncio.gather(a(), b())", "language": "python"}'
```

### Validation against real-world sources (2026-05-02)

The detectors have been smoke-tested against the prospector's MCP staging sources:

| Source | Findings | Comment |
|--------|----------|---------|
| MCP-001 (LangGraph parallel-interrupt) | 1 (`asyncio.gather` at line 254, dynamic args) | Correct — branch count `null` because args are spread. |
| MCP-002 (langgraphjs AsyncLocalStorage) | 2 (`Promise.all` at lines 2144, 2155) | Correct — non-literal iterables; flagged conservatively. |
| MCP-005 (mcp-server-memory file race) | 0 | **Correct.** MCP-005's race is implicit; the contention is in the host that calls the manager twice. The detector does not produce false positives — manual `instances[]` is required for this shape. |

The MCP-005 silence is the expected B1 behaviour: detectors fire on syntactic markers, not on shared-resource shape inference (B3). For implicit races today, hand-author the composition block per the [Composition](Composition.md) and [resources](#resources) guidance.

## Out of scope

- **Shared-label inference** (B2) — the Phase B output has empty `shared[]`. The user picks sync labels manually.
- **Resource-shape detection** (B3) — implicit-race shapes (MCP-005-style two-writer-no-gather) require points-to / call-graph reasoning the cursor-walk doesn't do. Filed as a separate gap.
- **Multi-file composition** — all classes referenced by `instances` must live in one source file. Cross-file class resolution is broader work.
- **Composition over more than ~10 instances** — the existing 2^18 state-space cap is the bound. The Step 0 degenerate-model warning will fire if you exceed it.

See also: [Composition](Composition.md) for the underlying CLTS semantics.
