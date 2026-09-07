# Adapter Formats

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change.

Mununu can import specifications from external formats and export synthesized controllers back to those formats. The adapter pipeline translates between external formats and the internal CTXDSL representation.

## Supported Formats

| Format | Extensions | Import | Export | Emission Mode |
|--------|-----------|--------|--------|--------------|
| **CTXDSL** | `.ctxdsl` | Native | Native | — |
| **TLSF** | `.tlsf` | Yes | No | Signal-state (turn-based) |
| **AIGER** | `.aag`, `.aig` | Yes | No | Signal-state (turn-based) |
| **BTOR2** | `.btor`, `.btor2` | Yes | No | Explicit automaton |
| **Promela** | `.pml`, `.promela` | Yes | No | Explicit automaton |
| **XState** | `.xstate`, `.json` | Yes | Yes | Explicit automaton |
| **SystemVerilog** (via Yosys) | `.sv`, `.v` (`--adapter sv-yosys`) | Yes | Yes | Explicit automaton |
| **Extraction Spec** | `.espec.json` | Yes | No | Explicit automaton |
| **CrewAI** | `.crewai.json` | Yes | No | Explicit automaton (per-agent + sequential supervisor + asynchronous composition) |
| **LangGraph** | `.langgraph.json` | Yes | No | Explicit automaton (nodes → states, edges → `node_<from>_enter` transitions) |
| **Microcode** | `.microcode.json` | Yes | No | Explicit automaton (steps → states, ops → labelled transitions) |

> The **Extraction Spec** adapter handles `.espec.json` files from the extraction pipeline (source code analysis). Properties can use `template_ref` to reference [Property Templates](Property-Templates) instead of raw mu-calculus formulas.
>
> The **CrewAI** and **LangGraph** adapters consume each framework's canonical JSON serialisation. See [Agentic Adapters](Agentic-Adapters) for the per-adapter semantics and `examples/verify/crewai_handoff/` and `examples/verify/langgraph_workflow/` for end-to-end fixtures.
>
> The **Microcode** adapter consumes a restricted JSON form documented under [Verify Project Flow](Verify-Project-Flow) (plan Part 5 + Part 5.5). One side-effect per step, explicit sequencing, resources declared up front, fences first-class, sharing tags on memory regions. See `examples/verify/rv5_2core_mesi_microcode_extracted/` (parity fixture) and `examples/verify/dma_engine_microcode/` (industrial DMA-engine demo).

## Pipeline

```
Source file → Adapter Parser → AdapterIR → CTXDSL Emitter → CLTS Realization → Synthesis
                                                                                    ↓
                                                               Controller → Native Format
```

---

## XState / Statecharts

Imports XState v5 JSON machine definitions. Supports simple, compound (hierarchical), and parallel states.

### Supported Features

- Simple states with event transitions
- Compound (nested) states — flattened with underscore-joined names
- Parallel states — each region becomes a separate automaton in synchronous composition
- Guards on transitions
- Context variables (boolean, bounded integer) as variable automata
- `__mununu` annotations for controllability and properties

### Controllability

Events are classified via the `__mununu` annotation block:

```json
{
  "__mununu": {
    "controllable": ["TIMER", "PROCESS"],
    "uncontrollable": ["USER_INPUT", "SENSOR"],
    "bounds": { "counter": [0, 10] },
    "properties": [
      { "name": "safety", "formula": "nu X. ([] X)", "role": "guarantee" }
    ]
  }
}
```

Events not classified default to **uncontrollable** (conservative).

### Limitations

- History states (shallow/deep) are not supported
- Delayed transitions (`after`) are not supported
- Arbitrary JavaScript actions are ignored — only `assign` with simple values is modeled
- String context variables are not supported
- Hierarchy is lost in round-trip (output is flat)

### Example

```json
{
  "id": "trafficLight",
  "initial": "green",
  "states": {
    "green": { "on": { "TIMER": "yellow" } },
    "yellow": { "on": { "TIMER": "red" } },
    "red": { "on": { "TIMER": "green" } }
  },
  "__mununu": {
    "controllable": ["TIMER"],
    "properties": [
      { "name": "safety", "formula": "nu X. ([] X)", "role": "guarantee" }
    ]
  }
}
```

### Controller Output

Synthesized controllers are emitted as flat XState v5 JSON with a `__mununu` metadata block:

```json
{
  "id": "trafficLight_controller",
  "initial": "green",
  "states": { ... },
  "__mununu": { "synthesis_result": "realizable", "generated_by": "mununu" }
}
```

---

## SystemVerilog

SystemVerilog (`.sv` / `.v`) is verified through the **`sv-yosys`** pipeline: `sv2v` normalises SV-2017 to Verilog-2005, Yosys elaborates with `hierarchy -check` (no `flatten`) and emits one BTOR2 per module, and mununu lifts each module to a KMTS by bit-blasting through the [BTOR2 reader](#btor2--yosys-rtl-phase-1). This is the **sole** SV path — the legacy hand-written FSM parser (inline `// @mununu` annotations, `--adapter sv`) was removed in S.2b.

Yosys handles the full synthesizable subset — `generate` blocks, parameter elaboration, packages, module instantiation, arrays/memories — so there is no hand-maintained "supported subset" table: whatever Yosys elaborates, mununu lifts (within the BTOR2 reader's `MAX_STATE_BITS` bit-blast cap; see [§BTOR2 + Yosys](#btor2--yosys-rtl-phase-1)).

### Abstraction posture (`.mununu.json` sidecar)

Abstraction is driven by a `.mununu.json` sidecar next to the source (not inline comments). It declares per-signal `FieldDomain` (Boolean / BoundedCounter / EnumValues / Ignored), `discovered_values`, init/memory abstractions, and controllability. See [RTL Verification Pipeline](RTL-Verification-Pipeline) for the schema and the authoring workflow — `mununu btor2 discover` seeds `discovered_values` via SMT predicate-image over the BTOR2 IR.

### Controllability

Derived from port directions: `input` ports → **uncontrollable** (environment), `output` ports → **controllable** (system). Override via the sidecar's `controllable` list.

### Multi-module composition

A top module that instantiates submodules composes structurally: each instance is lifted to a KMTS, its ports renamed to the connected nets (from the Yosys netlist), and the instances synchronously composed. Opt in with `[sources.options] multi_module = true` (+ optional `top`) on a `verify.toml` source. The composed automaton is named `Circuit`.

---

## BTOR2 + Yosys (RTL Phase 1)

BTOR2 (Niemetz–Preiner–Wolf, FMCAD 2018) is the de facto open-source word-level verification IR. Mununu's BTOR2 adapter parses the file, bit-blasts state and input bit-vectors into explicit valuations, and turns each `bad` / `constraint` / `fair` / `justice` line into a μ-calculus property. The **Yosys driver** (`adapter::yosys`) chains a child-process Yosys invocation onto the BTOR2 reader so a `.sv` file flows end-to-end as `SV → Yosys → BTOR2 → CLTS`.

### Pipeline

```
.sv  ──►  yosys (child process)  ──►  design.btor  ──►  BTOR2 reader  ──►  AdapterIR  ──►  CTXDSL  ──►  CLTS
                  │                                            │
                  └─ async2sync, chformal -lower, …            └─ per-signal labels, drop clock,
                                                                   filter chformal latches
```

**Yosys script** (`adapter::yosys::build_script`):

```text
read_verilog -formal -sv <source.sv>;
hierarchy -auto-top;
proc; flatten;
async2sync;          # NOT clk2fflogic — preserves the synchronous structure
chformal -lower;     # SVA → bad / constraint / fair / justice
dffunmap;
setundef -zero;      # X → 0; bit-blaster does not model X-prop
write_btor design.btor
```

### How the BTOR2 reader treats the design

Four CLTS-aligned transformations applied at read time (per [`adapter::btor2::bit_blast`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/btor2/bit_blast.rs)):

1. **Per-signal labels.** Each transition carries one `<signal>=<value>` label per non-clock input — `transition s0 -> s253 on label rst_0;` — using mununu's native multi-label transition support. Properties refer to individual signals via `[(rst_0)] φ` rather than enumerating compound `env_NNNN` strings. The CLTS alphabet shrinks from `2^N` (compound) to `2N` (per-signal-value) entries.
2. **Implicit clock.** Each CLTS transition represents one clock edge; `clk` does not appear in the alphabet. The reader auto-detects clock-shaped input names (`clk`, `clock`, `ck`, `clk_i`, `i_clk`, `iclk`, `clki`) and elides them from enumeration. Multi-clock and negedge designs are out of scope for Phase 1 — the reader errors explicitly.
3. **Synchronous Yosys script.** `async2sync` (rather than `clk2fflogic`) preserves the synchronous structure. `clk2fflogic` would introduce a `value + shadow + previous-clk` triple-state-cell encoding per FF group; `async2sync` produces one BTOR2 state cell per FF, matching mununu's "transition = one clock edge" semantics natively.
4. **Enumerated state names + valuations side-channel.** State names are `s0, s1, …, sN-1`. Per-state register valuations are carried via the existing `StateSpec.valuations` side-channel (the same mechanism the SV adapter uses) so user-written formulas like `state == 0` resolve via the on-demand expression evaluator. Synthetic `chformal -lower` property-tracking latches are filtered out of valuations so user formulas don't reference them.

### Yosys SVA support

Yosys 0.59's `read_verilog -formal -sv` is a synthesis frontend, not a full SystemVerilog assertion frontend. It accepts:

- Immediate assertions inside `always @(posedge clk)`: `assert (boolean_expr);`
- Immediate assumes / covers: `assume (...)`, `cover (...)`

It does **not** accept temporal SVA: `assert property (...)`, `|->`, `|=>`, `##N`, `s_eventually`, `$stable`, `$rose`, `$past`, `nexttime`, `default clocking`, PSL/FL syntax. The full SVA story arrives in **Phase 2** of the [RTL roadmap](RTL-Verification-Pipeline) (`adapter/sva/`), independent of the Yosys frontend. Until then, properties needing temporal operators are encoded with **shadow-register patterns** — a pre-cycle latch tracks the antecedent, an immediate Boolean assertion tests the consequent. This mirrors how every working OSS Yosys SVA example is actually written. See `examples/btor2/` in the repo for a corpus.

### State-bit budget

The bit-blaster caps total state at `MAX_STATE_BITS = 16` (≈ 65 K explicit states) and inputs at `MAX_INPUT_BITS = 10` per step. Beyond that, the design is rejected with `StateSpaceOverflow` — the documented escape hatch is compose-and-decompose (Phase 3) before BTOR2 hand-off to an external symbolic engine (Pono / AVR / BtorMC).

### CLI

```bash
# Drive Yosys end-to-end:
mununu context eval design.sv --adapter sv-yosys --formula safety_bad_0 --automaton Circuit

# Or hand mununu an existing BTOR2 file:
mununu context eval design.btor --adapter btor2 --formula safety_bad_0 --automaton Circuit

# SystemVerilog (.sv / .v) is verified via the sv-yosys pipeline (--adapter sv-yosys); .btor / .btor2 go through the BTOR2 reader directly.
```

### Soundness

- **Bit-blasting is exact** for the operators marked `is_blastable()` in [`btor2::ast::Op`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/btor2/ast.rs).
- **Implicit clock is sound for posedge-only single-clock designs** — multi-clock and negedge are explicitly rejected at read time.
- **`async2sync` preserves synchronous structure** for both register cells and `chformal`-lowered assertions.
- **`setundef -zero`** in the Yosys script makes X / undef bits deterministic; X-aware verification is out of mununu's roadmap scope.
- **Verific check:** the driver refuses to use a Yosys binary built with the commercial Verific frontend (license-incompatible).

See the in-repo `examples/btor2/README.md` for a concrete corpus walkthrough (`safety_demo`, `traffic_light`, `bounded_counter_with_assume`, `fair_arbiter`, `handshake_protocol`).

---

## Adapter Architecture

All adapters follow the same three-stage pipeline via the `FormatAdapter` trait:

```
Source file → Format Parser → AdapterIR → CTXDSL Emitter → CLTS
```

Each adapter implements:
- `detect(content)` — content-based format detection heuristic
- `translate(content, options)` — full parsing and translation pipeline

Auto-detection works by file extension (e.g. `.sv`, `.tlsf`, `.aag`, `.pml`, `.xstate.json`, `.espec.json`) and by content inspection.

---

## CLI Usage

### Import

```bash
# Auto-detect format from extension
mununu context eval design.sv --formula safety --automaton FSM
mununu context eval support_pipeline.xstate.json \
    --formula safety_invariant --automaton support_pipeline_system

# Explicit adapter selection
mununu context eval machine.json --adapter xstate --formula safety --automaton light
mununu context eval design.sv --adapter sv-yosys --formula safety --automaton Circuit
mununu context eval spec.espec.json --adapter extraction --formula safety --automaton Main
```

### Inspecting the intermediate CTXDSL

When using an adapter, add `--print-ctxdsl` to see the translated CTXDSL model:

```bash
# Print to stdout (alongside normal verification output)
mununu context eval design.sv --adapter sv-yosys --formula safety --automaton Circuit --print-ctxdsl

# Write to a file
mununu context eval design.sv --adapter sv-yosys --formula safety --automaton Circuit --print-ctxdsl output.ctxdsl
```

This works with all adapters (`--adapter tlsf`, `--adapter sv-yosys`, `--adapter xstate`, etc.) and with both `context eval` and `context synth`. The CTXDSL is printed before verification runs, so you can inspect the model even if verification fails.

### SystemVerilog Pipeline

The sole SV pipeline is `sv-yosys` (sv2v → Yosys → BTOR2 → bit-blast).
Author the `.mununu.json` sidecar by hand, or seed its `discovered_values`
via `mununu btor2 discover` (S.2b replaced the native `sv init`/`sv discover`
with BTOR2-IR discovery):

```bash
# (Optional) seed discovered values into the sidecar from the BTOR2 IR
mununu sv emit-btor2-per-module design.sv --output-dir build/
mununu btor2 discover build/design.btor2

# Verify with sidecar (auto-loaded from <stem>.mununu.json); the bit-blast
# names the composed automaton `Circuit`.
mununu context eval design.sv --adapter sv-yosys --formula safety --automaton Circuit
```

See [RTL Verification Pipeline](RTL-Verification-Pipeline) for the full annotation workflow.

### Export

```bash
# Export controller as XState JSON
mununu context synth machine.xstate --adapter xstate \
    --formula safety --automaton Machine \
    --output-format xstate --emit-native controller.json

# Export controller as SystemVerilog module
mununu context synth design.sv --adapter sv-yosys \
    --formula safety --automaton Circuit \
    --output-format systemverilog --emit-native controller.sv
```

---

## API Usage

### Import Endpoint

`POST /api/v1/context/import`

Translates external format content to CTXDSL.

**Request:**
```json
{
  "content": "<raw file content>",
  "format": "auto",
  "filename": "design.sv"
}
```

**Response:**
```json
{
  "success": true,
  "ctxdsl": "<translated CTXDSL>",
  "source_format": "SystemVerilog",
  "warnings": [],
  "signal_count": 4,
  "state_count": 0,
  "property_count": 1
}
```

### Synthesis with Native Output

`POST /api/v1/context/synthesize` with `output_format` in options:

```json
{
  "context": { "name": "design.ctxdsl", "content": "..." },
  "formula": "safety",
  "automaton": "FSM",
  "options": { "output_format": "xstate" }
}
```

The response includes `controller_native` with the format-specific output.

## See Also

- [Agentic Orchestration](Agentic-Orchestration) -- verify multi-agent workflows, MCP authorization, and handoff protocols
- [CLI Reference](CLI-Reference.md) -- full CLI documentation
- [API Reference](API-Reference.md) -- REST API documentation
- [Hardware Verification Patterns](Hardware-Verification-Patterns.md) -- RTL verification examples
