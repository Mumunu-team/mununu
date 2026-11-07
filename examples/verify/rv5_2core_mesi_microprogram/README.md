# `rv5_2core_mesi_microprogram` — smallest first slice of the multicore RISC-V scenario

> **Source of truth:** [`crates/mununu-core/src/verify/`](../../../crates/mununu-core/src/verify/) (verify framework) and [`docs/abstraction.md`](../../../docs/abstraction.md) (per-subsystem abstraction recipe) — surface: CLI+API+UI.

The minimum slice of the 4-core RISC-V verification plan (`i-want-you-to-distributed-orbit.md`, an untracked note) that proves the framework's bones for the full case. Two cores, one tracked cache line, MESI coherence, shared memory, a 3-step microprogram (`core_0` store → `fence_rw` → `core_1` load). No PLIC, no watchdog, no pipelines.

## What it demonstrates

- **N-source composition** of four `ctxdsl` sources composed asynchronously with rendezvous on the bus event labels (microprogram + per-core L1 caches + shared memory).
- **The MESI abstraction recipe** from `docs/abstraction.md` applied concretely: each cache is a 4-state symbol set (`I`, `S`, `E`, `M`) for the single tracked line; memory is a 2-state symbol set (`Mem_Initial`, `Mem_Written`); registers and addresses are not tracked.
- **The controllability-ownership convention** for shared rendezvous labels: the microprogram owns every bus label (because it is the controller); cache and memory automata reference the labels in their transitions but do not re-declare them.
- **Three property templates exercised on the composition**:
  - `mutual_exclusion(Core0_M, Core1_M)` — cache-coherence safety invariant (no two caches in `M` for the same line).
  - `reachable(Mem_Written)` — every store is visible to memory (reachability witness).
  - `reachable(MP_AfterLoad)` — the microprogram runs to completion (liveness reachability).

## State space

14 reachable states under asynchronous composition of `4 (microprogram) × 4 (Core0 cache) × 4 (Core1 cache) × 2 (memory)` = 128 cross-product states pruned to 14 reachable. Mununu's bitvec evaluator handles this in the millisecond range. Tractable headroom for the staged extensions (more lines, PLIC, watchdog) described in the plan.

## Files

| File | Purpose |
|---|---|
| `microprogram.ctxdsl` | 4-state microprogram (the verification target) + the canonical controllability declaration for every bus label |
| `l1_cache_core0.ctxdsl` | Per-core L1 cache for core 0; 4 MESI states; transitions on local + snoop labels |
| `l1_cache_core1.ctxdsl` | Mirror of `l1_cache_core0` with core indices swapped |
| `memory.ctxdsl` | 2-state shared-memory model (`Initial` / `Written`) |
| `verify.toml` | Project config (4 sources + asynchronous composition + 3 properties) |
| `validate.sh` | End-to-end reproduction script |
| `transcript.txt` | Byte-deterministic expected output |

## Reproduce

```bash
bash examples/verify/rv5_2core_mesi_microprogram/validate.sh
```

Re-running against the same commit produces a byte-identical `transcript.txt`.

## Run manually

```bash
mununu verify examples/verify/rv5_2core_mesi_microprogram/verify.toml
mununu verify examples/verify/rv5_2core_mesi_microprogram/verify.toml --json
mununu verify examples/verify/rv5_2core_mesi_microprogram/verify.toml --strict
```

## What this slice deliberately does not cover

- **Pipeline modelling.** A real RV5 5-stage in-order pipeline would add ~200-400 LOC of CTXDSL per core. Cycle-accurate hazards and forwarding paths are out of scope for the framework (mununu is event-driven, not cycle-accurate). See `docs/abstraction.md` for the abstraction recipe.
- **PLIC + interrupts.** Modelled separately in the full RISC-V plan via a parameterised library template (gap #2 from the plan's Part 6).
- **Critical subsystems** (watchdog, DMA, MMU, debug). Hand-authored per-system.
- **More than one tracked cache line.** Multi-line MESI grows as `5^(N×4)` for `N` lines × 4 cores; 2 lines = 625² ≈ 390k states (tractable but starts hurting). The pattern extends trivially; the state-space ceiling is the only constraint.
- **Weak-memory ordering (RVWMO).** Not encoded anywhere in mununu. Verifying "this `sw` + `fence` + `lw` sequence is correct under RVWMO" requires either an external memory-model checker (Herd, RMEM) integrated as an adapter or a heavy abstraction (TSO/SC). See plan Part 3 gap #9.

## Why this is the right "first thing to ship"

It is the smallest reproducible end-to-end demonstration that the verify framework composes a non-trivial cache-coherent multicore model and evaluates the safety + reachability properties that matter for memory-ordering verification. Every subsequent extension named in the plan (more cores, more lines, a real pipeline, a PLIC, a microcode adapter) preserves this slice's pattern and inherits its scaffolding.
