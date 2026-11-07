# `dma_engine_microcode` — industrial-impactful microcode-adapter demo

> **Source of truth:** [`crates/mununu-core/src/adapter/microcode/`](../../../crates/mununu-core/src/adapter/microcode/) — surface: CLI+API+UI.

The industrial-impactful significant extraction named in plan Part 5.5 (`i-want-you-to-distributed-orbit.md`, an untracked note): a minimal DMA-engine microcode composed with a tracked-memory automaton and an IRQ-controller stub.

Every SoC ships a DMA engine. Every DMA engine fetches a descriptor, writes a payload, flushes, signals completion, acknowledges its interrupt. The safety properties verified here — **descriptor-fetch-leads-to-completion**, **payload-eventually-written**, **interrupt-can-clear** — are the canonical DMA correctness invariants every vendor must guarantee. The microcode adapter's job is to make those properties extractable from a 30-line JSON description that an upstream tool (Bluespec, Chisel, an in-house microcode synthesiser) can emit mechanically.

## What it demonstrates

- **The microcode adapter on a real industrial pattern**, not a parity smoke test. The DMA channel's 6-step sequence is shape-identical to what every commercial DMA engine implements.
- **Multi-source composition with the microcode adapter** as the central source. Three sources (microcode + memory + IRQ controller) compose into a 9-state cross-product. All four properties hold.
- **Canonical industrial property templates exercised**:
  - `bounded_handoff(DMA_FetchDescriptor, DMA_SignalCompletion)` — once the DMA reads a descriptor, the completion step is reachable. The "no descriptor leaks into the void" property.
  - `reachable(Mem_PayloadWritten)` — the payload eventually commits to memory.
  - `reachable(IRQ_Cleared)` — the interrupt eventually clears.
  - `reachable(DMA_AckInterrupt)` — the DMA reaches its terminal ack step in the composed system.
- **`mununu verify --print-alphabet` on the composed system** — the transcript's second half lists every per-automaton alphabet, every reachable composed state, and every declared predicate. The user can author additional properties against this surface without rerunning verification.

## Files

| File | Purpose |
|---|---|
| `dma_channel.microcode.json` | 6-step DMA microcode (Idle → FetchDescriptor → WritePayload → FlushBuffer → SignalCompletion → AckInterrupt → loop). ~30 LOC of JSON. |
| `memory.ctxdsl` | 3-state shared-memory model tracking descriptor + payload regions |
| `irq_controller.ctxdsl` | 2-state IRQ controller (Pending / Cleared) |
| `verify.toml` | 3 sources + asynchronous composition + 4 properties |
| `validate.sh` | End-to-end reproduction script |
| `transcript.txt` | Byte-deterministic expected output including the `--print-alphabet` introspection |

## Reproduce

```bash
bash examples/verify/dma_engine_microcode/validate.sh
```

## What this demo deliberately does not cover

- **Multiple concurrent DMA channels.** Realistic for full SoC verification; queued as plan Part 6 item 6 (parameterised-instance support in `verify.toml`) makes this trivial.
- **AXI bus protocol details** (burst ordering, write-acknowledge, outstanding-transaction tracking). The shape extends naturally — add a bus arbiter automaton; the property templates already exist for it.
- **Descriptor-chain traversal** (the DMA fetches a chain of descriptors, not one). Today's microcode discipline supports labelled-`next` edges; loop bounds require the v1.1 extension named in the plan.
- **Per-byte data integrity** beyond "the payload region transitioned to `Written`". This is the standard CLTS abstraction trade-off — value-level integrity needs a richer model than mununu's event-driven semantics.

## Why this is the right industrial demo

DMA-engine correctness is **a multi-billion-dollar industrial concern**. Data corruption from DMA misordering is the kind of bug that ships in CPUs, GPUs, SmartNICs, and SSD controllers. Every formal-verification team at every IP vendor has a DMA-engine model in their regression suite. The shape of this demo's CTXDSL + JSON is what mununu offers them: a 30-line microcode description, two small support automata, four template-based property assertions, **byte-deterministic verification under a second**.

The same shape lifts to crypto round microcode, memory-controller refresh sequencing, and power-management firmware — the other three v1-target use cases named in plan Part 5.5.
