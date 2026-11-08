# Scoping log — C extraction principled-lift (Phase L0)

> Status: historical — a planning note published 2026-11-08 from the private planning tree (roadmap G4), unchanged except the banner and one line that linked a machine-local path to a plan that no longer exists. It records what was planned and measured at the time; the code and the design notes under `docs/design/` are the live truth.

> **Plan:** the Phase L0 planning note it was logged against (a private plan that no longer exists).
> **Triggering note:** [`docs/design/c-extraction-correctness-scope.md`](../../../docs/design/c-extraction-correctness-scope.md) §4.5
> **Gate:** the §4.5 "stop and evaluate before adding more slices" rule — this log records the evaluation that authorises switching from the AST-pattern approach to the principled LLVM-IR lift.
> **Date:** 2026-05-14
> **Reviewer:** Claude Opus 4.7 (1M context) on behalf of Mariano Cerrutti
> **Verdict:** **GREEN — proceed with phase L1.**

## Which §4.5 trigger fired

The scope doc names three triggers that would justify the switch. Two of the three have fired:

| Trigger | Status | Evidence |
|---|---|---|
| **(1) A real customer case exposes a soundness gap the AST approach cannot reasonably close.** | Partial. | Example 1 (macro base pointer, in plan §"Motivating examples") is the canonical STM32/NXP/Microchip HAL convention. The current `extern volatile T *const NAME` work-around in `examples/industrial/codesign_uart/firmware.c` is documented as a friction point; every existing MCU SDK uses the `#define` form the AST extractor cannot handle. |
| **(2) The slice queue grows past ~3 without a clear motivating example.** | **Fired.** | The plan enumerates six motivating examples (macro base pointer, type-cast register access, bit-field/union punning, helper function, ISR + main-thread, multi-entry driver) — these are *not* hypothetical, they are real firmware idioms that AST pattern matching either cannot represent or represents only with significant friction. The scope doc named six potential slices (2.c+, 2.d, 2.e, 2.f, 2.g, 2.h); each maps to one of the motivating examples. The threshold of three is well past. |
| **(3) A second target language joins that needs the same C-like extraction shape.** | Not fired. | C++ / Rust firmware / embedded Go are not on the immediate horizon. This trigger remains latent. |

Trigger (2) is the dominant one. The plan commits to the switch on that basis.

## Re-read of `docs/design/c-extraction-correctness-scope.md`

The scope doc was authored alongside slice 2.c (PR #38). Its §4 sketches the principled approach in conceptual terms:

1. Compile C to LLVM bitcode (`-O0`, `-emit-llvm`).
2. Build the CFG from basic blocks + branches.
3. Identify register accesses via `load`/`store` pointer-operand resolution (GEP, `inttoptr`, `getelementptr` on globals).
4. Quotient the CFG by register-access equivalence.
5. Emit on the existing rendezvous-label alphabet.

The plan operationalises this five-step recipe as phases L1–L3 (minimum viable parity) followed by L4–L7 (the motivating-example gaps). No conceptual gap between the scope doc and the plan. All five recipe steps map onto an L-phase.

## Re-verification of code anchors

| Anchor named in the plan | Status on `main` | Notes |
|---|---|---|
| `crates/mununu-extract/src/llvm.rs` (848 LOC, regex parser) | ✓ exists | Will factor out the language-neutral parts into a shared module/crate at phase L1. Currently tuned for `rustc --emit=llvm-ir` (`%self`-rooted GEPs, specific `define` shape); C extraction needs extensions but the base types (`LlvmModule`, `BasicBlock`, `Instruction`, `Terminator`) are reusable. |
| `crates/mununu-core/src/codesign/c_extract.rs` | ✓ exists | Slice 2.a/2.b/2.c lives here. Phase L3 deletes this file in the same PR that lands the LLVM backend's polling-loop detection. |
| `crates/mununu-core/src/codesign/coupling.rs::rendezvous_label_name` | ✓ exists | The LLVM backend emits the same labels — no downstream consumer change. |
| `crates/mununu-core/src/codesign/register_map.rs` | ✓ exists | The LLVM backend matches by `[base + offset, base + offset + width]` rather than by `c_accessor` string. The `Field` struct already carries `bits: [u32; 2]` for bit-range matching. |
| `crates/mununu-core/src/mununu_annotations/mod.rs` | ✓ exists | Phase L6 adds `@mununu_isr` to this grammar; precedent in slice 1 (PR #33) established. |
| `examples/industrial/codesign_uart/firmware.c` | ✓ exists | The phase L3 parity gate runs the LLVM backend against this file. Phase L3 also reverts the `extern volatile T *const UART` workaround to the `#define UART (...)` form. |
| `composition/mod.rs` asynchronous-composition path | ✓ exists | Phase L6 reuses this for the ISR composition — same primitive the codesign coupling already uses. |

All anchors resolve. No drift.

## Decisions on file (user-approved 2026-05-14)

1. **Shared IR parser**: factor `LlvmModule`/`BasicBlock`/`Instruction`/`Terminator` + the regex parser into a shared crate/module. Both `mununu-extract` (Rust) and `codesign::c_extract` (C) depend on it.
2. **Bit-field semantics**: per-field accesses. Register-wide store → N `RegisterAccess` entries for N overlapping fields. No `--coarse-bitfields` flag.
3. **ISR detection**: annotation-only via `@mununu_isr`. No naming-convention defaults.
4. **AST backend deprecation**: removed at phase L3, not deferred. No dual-surface period.

## Phase ordering — recap

| Phase | Scope | Standalone value |
|---|---|---|
| L0 (this log) | Scoping pass | Records the trigger + verdict; gates phase L1. |
| L1 | Shell out to `clang -emit-llvm`, parse IR, factor shared parser | Foundation. Emits a structural IR dump; no synthesis yet. |
| L2 | Register-access identification via address-range matching + linear synthesis | Closes examples 1 + 2 (macro base pointer, type-cast access). |
| L3 | CFG loop detection + polling-loop synthesis + **AST backend removal** | Parity gate against slice 2.c. AST extractor deleted in the same PR. |
| L4 | Bit-field per-field semantics | Closes example 3. |
| L5 | Interprocedural call-graph walk | Closes example 4 (helper functions). |
| L6 | `@mununu_isr` + asynchronous composition | Closes example 5 (ISR + main-thread). |
| L7 | Multi-entry driver composition | Closes example 6. |

## Risks acknowledged

1. **Phase L3 concentrates parity validation.** The PR must include (a) the same automaton output as slice 2.c on `firmware.c`, (b) the same output on a new `firmware_macro.c` using `#define UART (...)`, (c) every existing slice-2.b/2.c test case re-validated at the IR level. If any of these fails, phase L3 does not land and the AST backend stays.
2. **Regex-based IR parsing is fragile.** The existing `llvm.rs` parser is regex-driven and tuned for one shape of Rust IR. C IR carries patterns it doesn't expect (`inttoptr` literals, `getelementptr` on global `@`-symbols, bit-field load-modify-store sequences). Phase L1 must demonstrate the parser handles these on real `.ll` fixtures before phase L2 attempts register-access identification.
3. **`-O0` may still optimise away polling.** The `volatile` keyword is the firmware programmer's responsibility, not the compiler's. If a real driver omits `volatile`, the IR will have the register read constant-folded out of the loop. The lift will then misclassify the loop as a non-polling loop. **Mitigation**: surface a structured warning when a `while` loop's condition references a non-volatile pointer that lands in the register-map address range.
4. **`build_espec` in `mununu-extract/src/llvm.rs` is Rust-specific.** The factored shared crate must isolate the parser from `build_espec`; otherwise the Rust extraction path bleeds into the C extraction surface. Phase L1 needs explicit module boundaries.

## Re-evaluation cadence

This log is a one-shot gate — the trigger (2) has fired and the switch is authorised. No re-read of the scope doc is required for phases L2 through L7. Phase L3, however, **must** re-run the parity gate before deleting the AST backend; that re-run is part of L3's PR review checklist, not a separate scoping pass.

## Status

- **Phase L0**: complete. This log is the deliverable.
- **Next**: phase L1.
