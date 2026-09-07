# RTL Verification Pipeline

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change.

Mununu has **one** RTL ingestion path: the **Yosys-driven `sv-yosys` pipeline** (sv2v → Yosys → BTOR2 → bit-blast). It elaborates the full SystemVerilog subset Yosys handles — generate blocks, parameter elaboration, packages, structural SV — and lifts the per-module BTOR2 to a KMTS. A `.mununu.json` annotation sidecar drives the abstraction posture (`FieldDomain` Boolean / BoundedCounter / EnumValues / Ignored, `discovered_values`, automatic cone-of-influence).

> **S.2b note.** The legacy hand-written SV parser (`--adapter sv`, `mununu sv init`, `mununu sv discover`) was **removed**. Its sidecar-authoring role is now hand-editing plus `mununu btor2 discover`, which runs SMT predicate-image discovery over the same BTOR2 IR the verify path uses and writes `discovered_values` into the sidecar. SystemVerilog has exactly one pipeline.

---

### Pipeline Overview

```
SV Source → sv2v → Yosys (hierarchy, no flatten) → BTOR2 (per module) → Load Sidecar → bit-blast (+ optional SMT discovery) → KMTS → Verify
```

| Step | Command | Input | Output |
|------|---------|-------|--------|
| 1. (optional) Emit BTOR2 | `mununu sv emit-btor2-per-module design.sv --output-dir build/` | `.sv` | per-module `.btor2` |
| 2. (optional) Discover | `mununu btor2 discover build/design.btor2` | `.btor2` + `.mununu.json` | Updated `discovered_values` |
| 3. Edit | (manual) | `.mununu.json` | Refined annotations |
| 4. Verify | `mununu context eval design.sv --adapter sv-yosys` (or a `verify.toml` source) | `.sv` + `.mununu.json` | Property results |

Requires `yosys` (and `sv2v` for SV-2017 constructs) on `PATH`. Steps 2-4 are iterative — refine annotations and properties based on verification results.

> **This page = the explicit bit-blast engine.** The BTOR2 IR also feeds the
> **predicate-cube** abstraction (states = predicate cubes, not register bit-blast),
> with automatic CEGAR refinement and 3-valued (`KleeneT`/`KleeneF`/`KleeneBot`)
> verdicts — see [Predicate-Cube CEGAR](Predicate-Cube-CEGAR). Both the bit-blast and
> predicate-cube paths flow through the same frontend-agnostic **STS-IR seam** and the
> one 3-valued evaluator. For the whole architecture — explicit vs symbolic (BDD, R-F5)
> engines, the IR layering, and how over/under/⊥ approximation + may/must edges operate —
> see [`docs/design/post-rf5-architecture.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/post-rf5-architecture.md).

---

## Quick Start

```bash
# 1. (optional) Seed discovered values into the sidecar from the BTOR2 IR
mununu sv emit-btor2-per-module examples/systemverilog/alu.sv --output-dir build/
mununu btor2 discover build/alu.btor2

# 2. Review and edit alu.mununu.json
#    - Adjust bounds, mark signals to preserve/ignore
#    - Mark wide registers as "discover" for SMT analysis

# 3. Verify (the bit-blast names the composed automaton `Circuit`)
mununu context eval examples/systemverilog/alu.sv \
    --adapter sv-yosys --formula safety --automaton Circuit

# 4. Inspect the intermediate CTXDSL
mununu context eval examples/systemverilog/alu.sv \
    --adapter sv-yosys --formula safety --automaton Circuit --print-ctxdsl
```

---

## Annotation File Reference (`.mununu.json`)

The sidecar file lives next to the `.sv` source (e.g., `fifo.sv` → `fifo.mununu.json`). It declares which signals to model, how to abstract them, and what properties to verify.

### Schema

```json
{
  "$schema": "mununu_sv_annotation_v1",
  "module": "fifo",
  "source": "fifo.sv",
  "signals": [ ... ],
  "inputs": [ ... ],
  "controllable": [ ... ],
  "properties": [ ... ],
  "discovered_values": { ... },
  "parameters": { ... }
}
```

### `signals` — Registers and Internal State

Each entry controls how an internal register contributes to the state space:

```json
{
  "name": "fill",
  "preserve": true,
  "abstraction": "bounded_counter",
  "bound": 4,
  "note": "FIFO fill level"
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | required | Register name (must match SV declaration) |
| `preserve` | bool | `true` | Include in state space? |
| `abstraction` | string | `"discover"` | Abstraction strategy (see below) |
| `bound` | int | — | Upper bound for `bounded_counter` |
| `variants` | string[] | — | Enum variant names for `enum` |
| `value_map` | object[] | — | Numeric mapping for `enum` (e.g., `[{"name": "IDLE", "value": 0}]`) |
| `combinational` | bool | `false` | Signal is a combinational wire (`assign`), not a register |
| `note` | string | — | Human-readable note |

#### Combinational outputs

Signals driven by `assign` statements (not `always_ff`) can be included in the state space with `"combinational": true`. Their value is computed from the combinational logic each cycle, not from sequential assignments. This lets properties reference wire values directly:

```json
{
  "name": "overlap",
  "abstraction": "boolean",
  "combinational": true,
  "note": "assign overlap = uart_sel && aes_sel"
}
```

State names include the wire value: `overlap_T_state_IDLE`, `overlap_F_state_AES_ACCESS`.

When combinational evaluation cannot determine a wire's value (e.g., comparison involving a catch-all enum variant), the wire defaults to `false` (conservative).

### `inputs` — Input Ports

Same fields as `signals`, but for input ports. Inputs become label dimensions (one label per value combination).

```json
{"name": "wr_en", "abstraction": "boolean"},
{"name": "cmd", "abstraction": "discover"},
{"name": "data_in", "preserve": false}
```

### Abstraction Strategies

| Strategy | Values | Use for |
|----------|--------|---------|
| `boolean` | `{false, true}` | 1-bit flags, enables, resets |
| `bounded_counter` | `{0, 1, ..., bound}` | Counters, fill levels, indices |
| `enum` | Named variants | FSM states, command opcodes |
| `discover` | SMT-discovered values + catch-all | Wide registers with significant constants |
| `bit_blast` | `{0, ..., 2^width - 1}` | Small registers (≤4 bits) kept at full precision |
| `ignored` | (excluded) | Data-path registers, buffers |

### `properties`

```json
{
  "id": "no_overflow",
  "formula": "nu X. (!fill_5 && [] X)",
  "description": "Fill never exceeds DEPTH",
  "role": "guarantee"
}
```

Roles: `"guarantee"` (default), `"assumption"`, `"standalone"`.

### `discovered_values`

Populated by `mununu btor2 discover`. Each entry maps a signal to its SMT-discovered significant values:

```json
"discovered_values": {
  "cmd": {
    "values": [
      {"value": 0, "name": "NOP", "from": "SMT: guard (cmd == 0) at line 0"},
      {"value": 1, "name": "LOAD", "from": "SMT: guard (cmd == 1) at line 0"}
    ],
    "catch_all": "OTHER"
  }
}
```

You can rename the `name` fields — re-running `btor2 discover` preserves user-given names.

### `parameters`

Override module `localparam` / `parameter` values:

```json
"parameters": {"DEPTH": 4}
```

---

## SMT Discovery

When a register is marked `"abstraction": "discover"`, the `mununu btor2 discover` command uses z3 bitvector theory (SMT predicate-image over the BTOR2 IR) to find concrete values that make guard conditions satisfiable — even through combinational logic.

**Example:** If `assign y = x * 4;` and a guard checks `if (y == 12)`, SMT discovers `x = 3`.

The discovery:
1. Collects all guard expressions from `always_ff` / `always_comb`
2. Traces dependencies through `assign` / `always_comb` definitions
3. Builds z3 BV formulas per guard
4. Enumerates satisfying values via blocking clauses (up to 32 per signal)
5. Merges with syntactically-found constants (case labels, direct comparisons)

**Requires:** `cargo build --features smt` (bundles z3; first build takes ~10 minutes).

---

## Worked Example: FIFO Overflow Bug

### The Bug

A FIFO controller missing `if (fill < DEPTH)` guard on writes — fill can exceed capacity.

**Buggy code** (`fifo_overflow_bug.sv`):
```systemverilog
WRITING: begin
    // BUG: no guard — increments fill even when already at DEPTH
    fill <= fill + 1;
    state <= IDLE;
end
```

**Sidecar** (`fifo_overflow_bug.mununu.json`):
```json
{
  "module": "fifo_overflow_bug",
  "signals": [
    {"name": "state", "abstraction": "enum", "variants": ["IDLE", "WRITING", "READING"]},
    {"name": "fill", "abstraction": "bounded_counter", "bound": 5}
  ],
  "inputs": [
    {"name": "wr_en", "abstraction": "boolean"},
    {"name": "rd_en", "abstraction": "boolean"}
  ],
  "properties": [
    {"id": "no_overflow", "formula": "nu X. (!fill_5 && [] X)"}
  ]
}
```

### Verification

```bash
# Buggy: UNREALIZABLE — overflow is reachable
mununu context synth fifo_overflow_bug.sv --adapter sv-yosys \
    --formula no_overflow --automaton Circuit
# → Realizable: no

# Fixed: REALIZABLE — guard prevents overflow
mununu context synth fifo_overflow_fixed.sv --adapter sv-yosys \
    --formula no_overflow --automaton Circuit
# → Realizable: yes
```

### The Fix

```systemverilog
WRITING: begin
    if (fill < DEPTH)        // FIX: guard prevents overflow
        fill <= fill + 1;
    state <= IDLE;
end
```

---

## Real-World Examples

### AXI-Lite Overlapping Transactions (Xilinx Bug)

Based on Xilinx's Vivado AXI-lite slave template bug ([ZipCPU](https://zipcpu.com/formal/2019/04/16/axi-mistakes.html)). The slave accepts new write transactions while the previous response (`bvalid`) is still pending.

- **Files:** `axilite_deadlock_bug.sv`, `axilite_deadlock_fixed.sv`
- **Property:** `no_overlap` — never accept a transaction while bvalid is asserted
- **Buggy:** unrealizable (overlapping states reachable)
- **Fixed:** realizable (ready signals gated by `!bvalid_r`)

### CWE-1245: FSM with Undefined States (Security)

Based on MITRE CWE-1245, with real CVEs (CVE-2024-21853, CVE-2024-24968). One-hot FSM without `default` branch — glitch creates absorbing undefined state that bypasses access control.

- **Files:** `cwe1245_fsm_bug.sv`, `cwe1245_fsm_fixed.sv`
- **Property:** `recoverable` — from any state, IDLE is always reachable
- **Buggy:** 3/4 states satisfy (UNDEF is stuck)
- **Fixed:** 4/4 states satisfy (default branch forces recovery)

---

## Limitations

- **Explicit-state only:** state space capped at 2^18 (262K states). No BDD/SAT backend. For wide-register designs that blow this cap, use the [Predicate-Cube CEGAR](Predicate-Cube-CEGAR) engine, which abstracts to `2^(predicate count)` instead of `2^(register bits)`.
- **Single module:** no submodule hierarchy or multi-clock domains.
- **Abstraction is manual:** users must annotate which registers to preserve and how.
- **SMT discovery is combinational-only:** doesn't trace through sequential (clock-edge) dependencies.
- **SMT provenance:** line numbers in `discovered_values.from` are approximate (parser limitation).

## See Also

- [Predicate-Cube CEGAR](Predicate-Cube-CEGAR) — the complementary predicate-abstraction engine for designs too large to bit-blast (3-valued `{T, F, ⊥}` verdicts + automatic refinement)
- [Adapter Formats — SystemVerilog](Adapter-Formats#systemverilog) — supported SV subset and inline annotations
- [Hardware Verification Patterns](Hardware-Verification-Patterns) — CTXDSL patterns for common hardware
- [CLI Reference](CLI-Reference) — `sv preprocess`, `sv emit-btor2-per-module`, `btor2 discover`, `context eval`, `context synth`
- [Controller Synthesis](Controller-Synthesis) — synthesizing controllers from specifications
