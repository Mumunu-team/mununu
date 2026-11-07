> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/design/c-extraction-correctness-scope.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# C extraction correctness scope (Doc C §C.5b)

> Status: historical note + bounded soundness statement for the
> shipped LLVM-IR-based C extractor. Cited from
> [`docs/design/hw-sw-codesign-extraction.md`](hw-sw-codesign-extraction.md)
> §C.5 and from the module-level comments in
> [`crates/mununu-core/src/codesign/c_extract_llvm.rs`](../../crates/mununu-core/src/codesign/c_extract_llvm.rs).
>
> **Update (phase L3)**: the project switched from the AST-pattern
> approach this document originally argued *against* to the
> principled LLVM-IR / CFG approach. The "stop and evaluate" gate
> §4.5 names fired with motivating-example trigger (2) — the queue
> of slice-2.c-and-beyond examples grew to six concrete cases, and
> the switch was authorised. The implementation roadmap is at
> `~/.claude/plans/i-want-you-to-distributed-orbit.md`; phases L1
> (IR parser), L2 (register-access matcher), L3 (polling-loop
> detection + bit-field RMW collapsing + AST backend removal) have
> shipped.
>
> Sections 1–3 below describe the *original* AST-pattern approach
> and its limitations — they're kept for historical context. Section
> 4 was the proposed alternative and is now the shipped backend.

## 1. What the C extractor currently does

The shipped pipeline is documented at
[`crates/mununu-core/src/codesign/c_extract.rs`](../../crates/mununu-core/src/codesign/c_extract.rs):

1. **Slice 1** (PR #33). Doxygen / single-line / `//` comment grammar
   for `@mununu_*` tags on C declarations.
2. **Slice 2.a** (PR #34). Subprocess shell-out to
   `clang -Xclang -ast-dump=json -fsyntax-only`. Lifts user-defined
   function declarations + their attached `@mununu_*` annotations.
3. **Slice 2.b** (PR #35). Walks each function's body looking for
   register-access expressions. Reconstructs C accessor strings from
   clang's `MemberExpr` / `DeclRefExpr` chains. Matches the strings
   against the supplied [`RegisterMap`](../../crates/mununu-core/src/codesign/register_map.rs)
   by exact equality on `c_accessor`. Classifies each match as a read
   or a write. Emits a linear CTXDSL automaton on
   [`coupling`](../../crates/mununu-core/src/codesign/coupling.rs)'s
   rendezvous-label alphabet. **Linearises all control flow** with a
   structured `NonLinearControlFlow` warning per occurrence.
4. **Slice 2.c** (this PR). Recognises the canonical
   `while (single_register_read) ;` (or `{}`) polling idiom as a
   *state-creating* construct: introduces a dedicated `Loop_i` state
   with a self-loop and a same-label exit, faithfully reproducing the
   Doc C §C.4 hand-authored shape. Any other while shape — non-trivial
   condition, side-effecting body — still falls back to slice-2.b
   linearisation.

## 2. What the extractor does *not* claim

Honest framing, in the same spirit as Doc A §2.iii ("the default is
sound, but never silent"):

- **No trace-set equivalence.** The synthesised automaton's set of
  reachable label sequences has *no formally established relationship*
  to the set of label sequences the C function would produce when
  compiled and executed. The extractor is over-approximating where it
  knows it is, but it has no proof that the abstraction is conservative
  for every C construct.
- **No semantic check on the C source.** Type resolution, `sizeof`
  computations, bit-field semantics, `volatile` ordering, memory
  barriers — all of these are *not* reasoned about. The matcher does
  text-level comparison on the reconstructed accessor string, not
  semantic comparison.
- **No preprocessor reasoning.** Macros are pre-expanded by clang
  before mununu sees the AST. `#define UART (...)` constructs that
  inline a base-pointer cast disappear; the extractor cannot match a
  register access through them. The work-around documented in
  [`examples/industrial/codesign_uart/firmware.c`](../../examples/industrial/codesign_uart/firmware.c)
  is to declare base pointers as `extern volatile T *const NAME` so
  the AST emits a real `DeclRefExpr`.
- ~~**No interprocedural reasoning.**~~ **Closed by phases L5 + L5.5.**
  L5 inlines callees defined in the same module at the call site: the
  callee's body is walked recursively and its register accesses are
  spliced into the caller's list. L5.5 closes the parameter-aliasing
  gap: when a callee takes a peripheral pointer parameter
  (`void uart_wait_idle(volatile UART_TypeDef *u)`), the alloca-store-
  load round-trip clang emits at `-O0` is tracked, the parameter SSA is
  bound to the caller's argument's resolved address, and the callee's
  register accesses surface in the caller's automaton. Real-clang
  validation: `motivating_examples/example_4_helper_function_pointer_param.c`.
- ~~**No interrupt / ISR semantics.**~~ **Closed by phase L6.** Functions
  annotated `@mununu_isr` are tagged in the function summary and
  composed asynchronously with the main-thread automaton via the
  emitted `composition_ctxdsl` block. Naming-convention defaults
  (`*_IRQHandler`, `*_isr`) are deliberately NOT honoured — annotation-
  only. Real-clang validation: `motivating_examples/example_5_isr_with_main_thread.c`.

Soundness *for safety properties* is preserved through three
mechanisms:

1. **Linearisation = over-approximation.** Walking a side-effecting
   loop body as if the branch were always taken admits *more*
   behaviours than the real code, never fewer. Safety verdicts under
   the synthesised automaton transfer to the real firmware.
2. **`NonLinearControlFlow` is loud.** Every linearised construct
   emits a structured warning naming the function, the construct kind,
   and the source line. The user is told.
3. **The hand-authored CTXDSL is always available.** `mununu codesign
   verify` consumes either the synthesised or the hand-authored model;
   when the user needs a property synthesis cannot represent, they
   bypass extraction entirely.

Soundness *for liveness properties* is **not** preserved — same rule
as Doc A §2.iii's chaotic stub, applied to the C-extraction layer.

## 3. The slices we have *not* shipped (the queue)

Roughly six recognisable constructs sit beyond slice 2.c. None are
queued; the project commits to evaluating each one against §4 of this
document before implementing it.

| Slice | Construct | Why it might earn a slot |
|---|---|---|
| 2.c+ | Other control-flow shapes: `if/else`, `for`, `do`, `switch` | A real firmware function uses one of these to gate a register access, and the linearisation loses information that matters for a property under verification. |
| 2.d | Function calls (intra-module) | A driver factors its work across several helper functions; the synthesised automaton today stops at the call boundary. |
| 2.e | ISR / interrupt entry | A real codesign property concerns the interaction between the main-thread firmware and an ISR running on the same MCU. |
| 2.f | Multi-function composition | A compilation unit defines several entry points (`uart_send`, `uart_recv`, `uart_init`); today only one becomes the automaton at a time. |
| 2.g | Volatile + bit-field type resolution | The register-map matcher relies on the user matching `c_accessor` strings verbatim; a real firmware might use `*(volatile uint32_t *)0x40010000` patterns that the matcher cannot handle. |
| 2.h | Preprocessor / macro handling | The extern-declaration workaround is a real friction point for users with existing MCU SDKs that define base pointers as macros. |

## 4. The principled alternative — LLVM-IR / CFG / predicate abstraction

If the pragmatic AST-pattern-matching approach proves insufficient —
either because a real customer case exposes a soundness gap, or
because the queue of pattern-specific slices becomes unmanageable —
the project switches to a principled lift via LLVM IR.

### 4.1 Shape of the alternative

1. **Compile the C source to LLVM bitcode**: `clang -O0 -emit-llvm -c
   firmware.c -o firmware.bc`. The `-O0` is essential — optimisations
   would collapse polling loops to nothing and bury register accesses
   behind constant-folding.
2. **Build the function's control-flow graph** from the bitcode using
   `llvm-sys` (or shell-out to `opt` + a custom analysis pass). Each
   basic block in the CFG is a sequence of LLVM instructions terminated
   by a branch.
3. **Identify register accesses** at the IR level: a `load` or `store`
   instruction whose operand is a `getelementptr` chain rooted at an
   extern global with the peripheral's base-address symbol. The IR-level
   identification *resolves* through preprocessor macros, `sizeof`,
   pointer arithmetic, and bit-field encodings — all of the things the
   current AST-level matcher cannot see.
4. **Abstract the CFG by register-access equivalence**: two CFG nodes
   are equivalent if they witness the same set of (register, kind)
   pairs at the same control-flow position. Quotient the CFG by this
   relation. The result is an automaton with the register-access
   labels as transitions and equivalence classes as states.
5. **Emit the abstracted automaton** as CTXDSL on the coupling
   module's rendezvous-label alphabet — same downstream shape as
   today's slice 2.b/2.c output.

### 4.2 Academic precedent

The pattern is well-established:

- **CPAchecker** (Beyer & Keremoglu, TACAS 2011) — configurable
  program analysis over LLVM IR with predicate abstraction. Open
  source. The closest off-the-shelf candidate.
- **SLAM** / **BLAST** (Ball, Rajamani; Henzinger et al., POPL 2002,
  SPIN 2003) — counterexample-guided abstraction refinement (CEGAR)
  over C source. The conceptual ancestor.
- **CIL / Coccinelle** (Lawall et al.) — semantic-patch frameworks
  for C, used in Linux-kernel code transformations. Demonstrate that
  semantic-level C analysis is industrially practical.
- **TCG / QEMU's IR** — for the embedded use case specifically,
  qemu's translation block IR is a precedent for abstracting register
  accesses at the IR level rather than at the source level.

### 4.3 What the principled lift would buy

| Today (AST + patterns) | Principled lift (LLVM IR + CFG) |
|---|---|
| Recognises constructs case-by-case. Each new construct is a slice. | Handles arbitrary C source with no per-construct work. |
| Soundness claim: over-approximation + safety + explicit warnings. | Soundness claim: provable over-approximation up to the abstraction's published bounds (typically: floating-point, dynamic allocation, function pointers — bounded out by `-O0`). |
| Preprocessor macros are work-arounds. | Preprocessor macros are transparent (clang's IR already has them expanded with full type information). |
| `volatile`, bit-fields, struct layout are opaque. | First-class — IR carries the alignment, the bit-offset, the access width. |
| Interprocedural reasoning needs slice 2.d. | Interprocedural reasoning is free (the CFG spans the call graph). |
| The hand-authored CTXDSL is the canonical fallback. | The synthesised CTXDSL is the canonical primary; the hand-authored form becomes a refinement. |

### 4.4 What the principled lift would cost

- **A new Rust dependency on LLVM.** Either `llvm-sys` (build-time
  matching of system LLVM version — fragile across distros) or
  a shell-out to `opt` with a custom analysis pass loaded as a `.so`
  (still requires LLVM dev headers on the host). The CMSIS-SVD
  importer (PR #32) and the clang shell-out (PR #34) are tractable
  precedents; an LLVM-pass dependency is heavier.
- **A predicate-abstraction implementation.** Either adopt
  CPAchecker as a subprocess (introduces a JVM dependency) or
  reimplement the core in Rust. Both are substantial chunks of work,
  not weekend slices.
- **A formal soundness proof.** The whole point of switching is to
  *upgrade* the soundness story from "useful + auditable" to
  "provably equivalent". Doing the lift without writing the proof
  buys little — the AST pattern-matching is already useful and
  auditable.
- **A new test suite.** A representative sample of real-world
  embedded C (Zephyr drivers, FreeRTOS BSP code, STM32Cube HAL) to
  exercise the lift against. Today's slice tests are synthetic AST
  fragments; the principled lift would need real C input.

### 4.5 When to take the switch

The project commits to evaluating the switch when *any one* of the
following triggers:

1. **A real customer case** exposes a soundness gap in the
   AST-pattern approach that a slice cannot reasonably close — i.e.
   the construct that breaks is not a single new pattern but a
   *family* of patterns the matcher cannot reach.
2. **The slice queue grows past ~3** without a clear motivating
   example. If we're adding slices because "we should handle this"
   rather than because "this real example broke," the AST approach
   is being asked to do work it isn't suited for.
3. **A second target language joins** that needs the same C-like
   extraction shape (C++, Rust firmware, embedded Go). The LLVM IR
   lift is language-agnostic at that level; the AST approach would
   need a parallel implementation per language.

Until one of those triggers fires, the recommendation is to ship
slices opportunistically when a concrete example forces one, document
the bounds (as this document does), and keep the hand-authored CTXDSL
as the canonical model for anything the synthesis cannot represent.

## 5. Status of this document

- **Authored**: 2026-05-14, alongside slice 2.c.
- **Cited from**:
  - [`docs/design/hw-sw-codesign-extraction.md`](hw-sw-codesign-extraction.md) §C.5b
    (sub-section to be added; this file is the canonical content).
  - [`crates/mununu-core/src/codesign/c_extract.rs`](../../crates/mununu-core/src/codesign/c_extract.rs)
    module-level documentation.
- **Re-evaluation cadence**: re-read this document before opening any
  PR that adds a slice beyond 2.c. If the re-read still passes — no
  trigger from §4.5 fires — the slice proceeds with explicit
  justification. If a trigger fires, the slice is paused and the
  principled-lift work is scoped out as a separate milestone.
