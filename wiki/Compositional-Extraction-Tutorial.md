# Compositional Extraction — Local Tutorial

Hands-on walkthrough for the compositional-extraction pipeline. Four paths:

1. **Path A — Public synthetic fixture** (anyone with the repo can run; the recommended starting point).
2. **Path B — Real MCP-server source** (uses local-only staging fixtures under `.claude/reviews/prospector/staging/`; demonstrates the workflow on real-world MCP-protocol implementations the prospector flagged).
3. **Path C — New source via Phase B auto-detection** (use `--propose-composition` to seed the composition block from any source file).
4. **Path D — Web UI hybrid** (CLI for extraction, mununu-ui for compose-block authoring + Phase B suggestions + visual verification).

For the schema, semantics, and per-instance label-rewriting algorithm, see [Compositional-Extraction](Compositional-Extraction.md). This page only covers running the pipeline.

## Prerequisites

```bash
# From the repo root
make build   # or: cargo build --release -p mununu-cli -p mununu-extract
```

The `mununu` and `mununu-extract` binaries land in `target/release/` and are invoked below as `cargo run --release -q -p <crate> -- <args>`. If you've added `target/release/` to your `PATH`, the bare names work too.

---

## Path A — Public synthetic fixture

The repo ships a minimal worker-race fixture under [examples/ast_extract/typescript/](https://github.com/Mumunu-team/mununu/tree/main/examples/ast_extract/typescript/). One `Worker` class with a `_committed` flag, plus a hand-modelled shared file. Two instances of the worker contending over the file is the smallest non-trivial composition that produces a real race witness.

### Files

| Path | Role |
|------|------|
| [examples/ast_extract/typescript/parallel_workers.ts](https://github.com/Mumunu-team/mununu/blob/main/examples/ast_extract/typescript/parallel_workers.ts) | Source code (one `Worker` class). |
| [examples/ast_extract/typescript/parallel_workers_compositional.extract.json](https://github.com/Mumunu-team/mununu/blob/main/examples/ast_extract/typescript/parallel_workers_compositional.extract.json) | Extract config with `composition.instances[]`, `composition.resources[]`, and two properties. |
| [examples/ast_extract/typescript/parallel_workers_compositional.expected.txt](https://github.com/Mumunu-team/mununu/blob/main/examples/ast_extract/typescript/parallel_workers_compositional.expected.txt) | Reference verdicts — diff your output against this. |

### Run it

```bash
# Step 1 — extract: source + config → multi-automaton espec.
cargo run --release -q -p mununu-extract -- ast \
    examples/ast_extract/typescript/parallel_workers_compositional.extract.json \
    --source examples/ast_extract/typescript/parallel_workers.ts \
    --output /tmp/parallel_workers.espec.json

# Expected stderr (the structure summary):
#   Extracted: 3 automata, 2 properties
#     worker_a — 2 states, 5 transitions
#     worker_b — 2 states, 5 transitions
#     shared_file — 4 states, 4 transitions

# Step 2 — eval the safety property (file never clobbered).
cargo run --release -q -p mununu-cli -- context eval \
    /tmp/parallel_workers.espec.json \
    --formula no_clobber --automaton two_writer_race
# Expected: 0/9 states satisfy. Race detected.

# Step 3 — eval the reachability witness (file CAN reach clobbered).
cargo run --release -q -p mununu-cli -- context eval \
    /tmp/parallel_workers.espec.json \
    --formula clobber_reachable --automaton two_writer_race
# Expected: 9/9 states satisfy. Initial 1/1 → non-vacuous.
```

### Interpreting the verdicts

The pair `(no_clobber fails, clobber_reachable holds)` is the **canonical race-detection signature**:

- `no_clobber` failing on its own would be ambiguous — could be a real race, or could be a vacuous failure on a 1-state model.
- `clobber_reachable` holding from the initial state confirms the model has enough structure for the verdict to mean something.
- The composed state has 9 reachable states (4 file states × 2 worker_a states × 2 worker_b states minus unreachable combinations). The transitions cross every interleaving of `worker_a__ev_commit` and `worker_b__ev_commit`.

If you see `(no_clobber fails, clobber_reachable also fails)`, the model collapsed to a single state and the verdict is vacuous — see GAP-009's vacuity warning. The synthetic fixture here is engineered to avoid this by giving each worker a non-trivial 2-state lifecycle.

### Try a fix

Edit the extract config to add `"shared": ["ev_commit"]` (replacing `"shared": []`). Now both workers' `commit` labels are kept verbatim and **must fire jointly** — the engine forces a single combined commit step. Re-extract and re-eval:

```bash
cargo run --release -q -p mununu-extract -- ast \
    examples/ast_extract/typescript/parallel_workers_compositional.extract.json \
    --source examples/ast_extract/typescript/parallel_workers.ts \
    --output /tmp/parallel_workers_fixed.espec.json

cargo run --release -q -p mununu-cli -- context eval \
    /tmp/parallel_workers_fixed.espec.json \
    --formula no_clobber --automaton two_writer_race
```

With shared commit, the race is gone (or the verdict changes to reflect the new topology). This is a useful exercise for building intuition: the same source code yields different verdicts depending on the composition contract you declare.

### Try CTXDSL

The same espec can be translated to a human-readable CTXDSL file and inspected:

```bash
cargo run --release -q -p mununu-cli -- context eval \
    /tmp/parallel_workers.espec.json \
    --formula no_clobber --automaton two_writer_race \
    --emit-ctxdsl /tmp/parallel_workers.ctxdsl

# Then read /tmp/parallel_workers.ctxdsl to see the generated DSL —
# the `composition { asynchronous two_writer_race { members [...] } }`
# block at the top is the bridge between the JSON spec and the CLTS
# verification engine.
```

---

## Path B — Real MCP-server source

The prospector backlog has surfaced four real-world MCP-protocol bugs, each with a hand-anchored extraction spec under `.claude/reviews/prospector/staging/<TARGET>/`. Those directories are **gitignored** (the `.claude/` tree holds session-local working files), but if your checkout has them, you can re-run them through the same compositional pipeline.

### Available targets (when present)

| Staging dir | Source under analysis | Bug shape |
|-------------|----------------------|-----------|
| `MCP-001` | `tool_node_prefix.py` (LangGraph) | Parallel-interrupt resume race. `composition.instances[]` over 2 worker copies + dispatcher resource. |
| `MCP-002` | `pregel_index.ts` (langgraphjs) | AsyncLocalStorage contamination. 2 store instances + shared async-context registry. |
| `MCP-003` | `traces_vulnerable.py` (Python MCP) | Trace-handler validation gap. Single-class today; not yet compositional. |
| `MCP-005` | `index.ts` (mcp-server-memory) | File-write race. 2 manager instances + shared `memoryFilePath`. |

### Run MCP-005 (the canonical fully-compositional fixture)

```bash
# Verify the staging dir exists in your checkout
test -d .claude/reviews/prospector/staging/MCP-005/ && echo "OK" || echo "MISSING — see Path A"

# Step 1 — extract using the prepared compositional config
cargo run --release -q -p mununu-extract -- ast \
    .claude/reviews/prospector/staging/MCP-005/manager_compositional_with_resources.extract.json \
    --source .claude/reviews/prospector/staging/MCP-005/source/index.ts \
    --output /tmp/mcp005_compose.espec.json
# Expected:
#   Extracted: 3 automata, 0 properties
#     worker_a — 1 states, 3 transitions   (degenerate — see GAP-009)
#     worker_b — 1 states, 3 transitions   (degenerate — see GAP-009)
#     shared_file — 3 states, 2 transitions
#   [mununu] WARN: automaton 'worker_a' is degenerate (1 states, 0 state-mutating transitions)...

# Step 2 — the auto-extracted espec doesn't ship properties; the
# refined version under staging does. Use that for eval:
cargo run --release -q -p mununu-cli -- context eval \
    .claude/reviews/prospector/staging/MCP-005/composed_with_resources_refined.espec.json \
    --formula no_clobber --automaton memory_write_race
# Expected: 0/1 — race detected.

cargo run --release -q -p mununu-cli -- context eval \
    .claude/reviews/prospector/staging/MCP-005/composed_with_resources_refined.espec.json \
    --formula clobber_reachable --automaton memory_write_race
# Expected: 1/1 — clobber reachable, witness valid.
```

### Why the MCP-005 model is "degenerate" but the verdict is still right

The **per-instance** automata collapse to 1 state because `KnowledgeGraphManager`'s only state field (`memoryFilePath`) takes a single value under presence-abstraction. Without the explicit `composition.resources[]` block, this would produce a vacuous `(0/1, 0/1)` verdict. With the resource block, the bug witness lives in the **shared_file resource's** state space (3 states: `v0`, `v1`, `clobbered`), and the verdict matches the hand-modeled baseline. This was the motivation for GAP-008 (resources schema) and GAP-009 (vacuity warning).

If you want to compare auto-extracted vs hand-baseline byte-for-byte:

```bash
diff /tmp/mcp005_compose.espec.json \
    .claude/reviews/prospector/staging/MCP-005/composed_with_resources.espec.json
```

The auto-extracted output should match the hand baseline modulo whitespace and the missing `properties[]` block (which is added during refinement).

### MCP-001 / MCP-002 / MCP-003

The other staging dirs follow the same shape:

```bash
ls .claude/reviews/prospector/staging/MCP-001/
# Look for *_compositional*.extract.json or *.espec.json
# Each fixture is documented in the staging dir's surrounding files
# (extract_*.txt logs, eval-*.txt verdicts).
```

The eval output captures (`eval-no_clobber-compositional.txt`, etc.) record the verdicts at the time the staging fixture was last refreshed. Re-running them is a regression check on the pipeline — if your verdicts diverge from the captured ones, either the model changed or a downstream component drifted.

---

## Path C — New source via Phase B auto-detection

For source code you haven't yet authored an extract config for, run the Phase B pre-pass to seed the `composition.instances[]` block:

```bash
# Step 1 — scan source for concurrency idioms
cargo run --release -q -p mununu-extract -- ast /dev/null \
    --source path/to/your/source.py \
    --propose-composition \
    --output /tmp/findings.json

# Step 2 — read /tmp/findings.json (one DetectedConcurrency per
# call site). Each finding has detector_id, line, branch_count,
# suggested_instance_names, suggested_class_hint.
cat /tmp/findings.json
```

The output is **suggestion-grade**: the user reviews each finding, picks one, and writes a real extract config around it. The Phase B pre-pass is not a replacement for authoring the composition block; it's a starting point.

The Web UI exposes the same flow as a one-click button — see [mununu-ui's local-testing guide](https://github.com/Mumunu-team/mununu-ui/blob/main/docs/phase-b-local-testing.md) for the UI walkthrough.

The Phase B detectors and the concurrency idioms they recognise live in [`concurrency_detect.rs`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/extraction/ast_extract/concurrency_detect.rs) — read the pattern matchers there for the exact coverage and its limits.

---

## Path D — Web UI

The mununu-ui drives the same compositional pipeline through visual editors. Two terminals + one browser:

### Step 0 — start backend + UI

```bash
# Terminal 1 — backend (port 8080)
cd ~/git_repo/mununu
cargo run --release -p mununu-cli -- server --addr 127.0.0.1:8080

# Terminal 2 — UI (port 5173)
cd ~/git_repo/mununu-ui
npm install   # first run only
npm run dev
# → http://localhost:5173
```

### Step 1 — drive the workflow

1. Open `http://localhost:5173`.
2. Sidebar → **Extraction** → **Software Extraction** workflow.
3. **Load Source**: drop or paste [examples/ast_extract/typescript/parallel_workers.ts](https://github.com/Mumunu-team/mununu/blob/main/examples/ast_extract/typescript/parallel_workers.ts). The file extension `.ts` is recognised → the Phase B `Suggest from source` button will appear in the compose step.
4. **Extract Model**: the step renders an `ExtractConfigEditor` JSON textarea.
   - Click **Start from template** to seed a default config (single-class targeting `Worker`, sourced from the loaded filename).
   - Replace the placeholder `targets[]` block with the real one — easiest path: open [parallel_workers_compositional.extract.json](https://github.com/Mumunu-team/mununu/blob/main/examples/ast_extract/typescript/parallel_workers_compositional.extract.json), copy the body, paste it into the editor.
   - The summary panel below the textarea reports `source.file`, `language`, `targets (N)`, `composition`, and `properties (N)` once the JSON is valid.
   - Click **Run extract**. The backend returns the multi-automaton espec (3 automata for the parallel-workers fixture: `worker_a`, `worker_b`, `shared_file`). The result message reads `Extracted 3 automaton/a. 0 warning(s).`
5. **Compose Instances**: visual editor over the composition sub-block. Three things you can do here:
   - Click **Start from template (2-instance race)** to see a worked compositional config rendered in the validating editor.
   - Click **Suggest from source (Phase B)** to scan `parallel_workers.ts` for concurrency idioms — the synthetic fixture has no `Promise.all` / `asyncio.gather` so this returns 0 findings (correct — Path A's source is intentionally minimal). Try the same button after loading a source with idioms (e.g., from `.claude/reviews/prospector/staging/MCP-001/source/tool_node_prefix.py`) and you'll see real findings with **Apply** buttons.
   - Author the composition JSON manually. The editor shows the live JSON-schema validation summary plus `Valid composition` confirmation when the shape is right.
   When you change the composition here and want it reflected in the extract config, go back to **Extract Model** and click **Sync composition from compose step** — it merges `compositionConfig` into `extractConfig.composition` in one click. Then re-run extract.
6. **Edit Spec** (optional): refine the auto-extracted espec (add properties, tighten abstractions, adjust `mode: vulnerable / fixed / both` filters) in the SidecarEditor.
7. **Translate**: calls the backend's `/context/import` endpoint and converts the espec into CTXDSL. The result is stored in `state.ctxdslContent` and previewed in the panel.
8. **Verify**: this step says "switch to the Verification tab" — that's where the actual evaluation happens.
9. Switch to the **Verification** tab in the sidebar. The CTXDSL content auto-loads from `state.ctxdslContent`. Pick the formula `no_clobber` and the automaton `two_writer_race`. Click **Verify**. You should see:
   - `no_clobber` over `two_writer_race`: **0/9 states satisfying, 0/1 initial states satisfying**, status FAILED — the race is reachable. The initial violating state is `clean|_committed_F|_committed_F`.
   - Switch to `clobber_reachable`: **9/9 states satisfying, 1/1 initial state satisfying**, status PASSED — the witness is non-vacuous.
   The Verification tab also renders a counterexample trace + counterstrategy graph for the failing property — useful for debugging the witness path.

### Step 2 — what each UI surface adds beyond the CLI

| UI surface | What it gives you that the CLI doesn't |
|------------|----------------------------------------|
| `ExtractConfigEditor` (extract step) | Live JSON validation against the extract-config schema; one-click template seeding from `sourceFileName`; one-click `Sync composition from compose step` so the two editors stay aligned; targets / composition / properties summary panel. |
| `CompositionEditor` (compose step) | Live JSON-schema validation; instance / shared-label preview rendering; explicit "Valid composition" confirmation; reduces the JSON-syntax-error → re-run-extractor cycle. |
| Phase B `Suggest from source` button | One-click pre-pass over the loaded source file with one-click `Apply` for each finding. Same backend as `mununu-extract --propose-composition` but with a click-to-apply UX. |
| SidecarEditor | Espec editor with .espec.json schema awareness; useful for refining the auto-extracted spec (adding properties, tightening abstractions, adjusting `mode: vulnerable / fixed / both` filters). |
| CTXDSL preview | See the generated CTXDSL with syntax highlighting; the `composition { asynchronous two_writer_race { members [worker_a, worker_b, shared_file] } }` block is the bridge between the extraction layer and the verification engine. |
| Verification tab | Counterexample trace + counterstrategy graph rendering for failing properties. The CLI's `mununu context eval` only prints state names; the UI walks you through the violating path step by step. |

---

## Troubleshooting

| Symptom | Diagnosis | Fix |
|---------|-----------|-----|
| `Extracted: ... 0 properties` | The extract config has no `properties[]` block, or properties failed to parse. | Add raw-formula properties (Path A) or run eval on the refined espec instead (Path B). |
| `unknown formula 'X' in realised context` | The eval command's `--formula X` doesn't match any property in the espec. | Either add the property to the extract config or pass the property `id` as `--formula`. Property template references are resolved at adapter translation time; the espec's `properties[].id` is what `--formula` expects. |
| `automaton 'X' is degenerate (1 states, ...)` (GAP-009 warning) | The per-instance automaton has only 1 reachable state — likely because the only state field uses a presence/Boolean abstraction that collapses. | If the bug witness lives in a shared resource (file, channel), this is fine — see Path B's MCP-005 example. Otherwise add more state fields to `state_fields.include`. |
| `Initial states satisfying: 0/1` for both `no_clobber` AND `clobber_reachable` | Vacuous verdict — the model is too small. | Check the structure summary's per-automaton state counts. If any automaton has 1 state and no shared resource is declared, the composition has no room for a witness. |
| Verdict differs from the expected file | The pipeline drifted (regression) or the source/config changed. | Diff the espec against the expected baseline, then re-extract from a known-good revision to localize. |

## Where each component lives

If you need to extend or debug the pipeline:

| Component | Location |
|-----------|----------|
| Extract-config schema | [crates/mununu-core/src/adapter/extraction/ast_extract/config.rs](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/extraction/ast_extract/config.rs) — `ExtractionConfig`, `CompositionConfig`, `ResourceDecl`. |
| Per-instance label rewriting | [crates/mununu-core/src/adapter/extraction/ast_extract/mod.rs](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/extraction/ast_extract/mod.rs) — `rewrite_labels_for_instance`. |
| Resource → automaton emission | Same file — `build_resource_automaton`. |
| Composition engine | [crates/mununu-core/src/composition/mod.rs](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/composition/mod.rs) — alphabet-intersection synchronization. |
| Phase B detectors | [crates/mununu-core/src/adapter/extraction/ast_extract/concurrency_detect.rs](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/extraction/ast_extract/concurrency_detect.rs). |
| Property templates | [crates/mununu-core/src/adapter/templates/builtin_templates.json](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/templates/builtin_templates.json) — `no_clobber`, `clobber_reachable`, `mutual_exclusion`, `bounded_handoff`, `no_lost_update`. |
