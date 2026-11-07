> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/design/hw-sw-codesign-extraction.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# HW/SW codesign — formal verification across the boundary

> **Status:** design
> **Audience:** mununu contributors and reviewers reasoning about how the contract subsystem extends into firmware + RTL co-verification.
> **Companion documents:** [A — Black-box modules in compositional extraction](black-box-modules.md), [B — RTL frontend unification](rtl-frontend-unification.md), [D — Contract corpus and unified config](contract-corpus-and-config.md). This document is the **capstone** of the four-document arc — it composes A's contract machinery, B's RTL pipelines, and D's corpus + annotations into one industrial use case.

## C.0 The pitch in one paragraph

Every embedded device is a coupled pair: a SystemVerilog peripheral with memory-mapped registers, and a C firmware that reads and writes those registers to drive the peripheral. The properties that bite in practice — "firmware never enables the peripheral mid-transaction," "no race between firmware reset and peripheral autonomous activity," "interrupt acknowledgement always reaches the controller within N cycles" — *cross the boundary*. Verifying each side in isolation misses them entirely. This document spells out the data model (two reactive modules + a register-map sidecar coupling them), the six-stage pipeline (SV extract → C extract → coupling synthesis → joint composition → verify → interleaved counterexample), the C-extraction tooling assessment (tree-sitter / libclang / LLVM), the soundness considerations (bus arbitration, interrupt latency, memory model), and the implementation plan that gets there one slice at a time.

## C.1 The problem

Two artefacts, one system:

- A **SystemVerilog peripheral** with memory-mapped registers (control, status, data FIFOs, interrupt flags). This is the work Document B's RTL pipelines extract today.
- A **C firmware** (interrupt handler, polling loop, DMA setup, register write sequences) that reads and writes those registers. This is *not* something mununu extracts today — the `mununu-extract` `ast` backend at [crates/mununu-extract/src/main.rs](../../crates/mununu-extract/src/main.rs) covers TypeScript, Python, and Rust; C is missing from the list.

Properties of interest cross the boundary:

- **Safety.** "Firmware never enables the peripheral while a previous transaction is in flight." Reads firmware-side ordering against peripheral-side state.
- **Reliability.** "Peripheral never raises an interrupt the firmware cannot acknowledge." Reads peripheral-side timing against firmware-side liveness.
- **Race-freedom.** "No race between firmware-driven reset and peripheral autonomous activity." Reads both sides' simultaneous access to shared state (registers).

Today mununu extracts each half separately, but the coupling — the register map and the access semantics — is not first-class. Verification is therefore *only per side*, which misses the cross-boundary properties that are exactly the ones a model checker would otherwise catch.

The argument for fixing this: HW/SW codesign is the **largest underserved population** for formal verification today. Hardware emulators (ZeBu, Palladium, Veloce) handle the simulation side; debugger-oriented tools (Verdi HW/SW Debug, IP-XACT tooling) handle the observability side; formal verification across the boundary, in the assume-guarantee discipline, is rare in industrial tooling. The contract machinery in Documents A and D provides the conceptual foundation; this document spells out what's needed to bring it to embedded codesign.

## C.2 Conceptual model — two reactive modules + a coupling

Borrowing the Reactive Modules frame (Alur & Henzinger, *Formal Methods in System Design* 1999): model SV and C as **two reactive modules that share a set of coupled variables**. Each register in the design is a coupled variable; the SV side and the C side both have read/write access governed by three orthogonal axes:

1. **Register direction.** RW (firmware reads and writes; peripheral may update on its own), RO (firmware reads only; peripheral updates the value), WO (firmware writes; peripheral reacts but does not return data through this register). Mirrors the IP-XACT / SystemRDL / CMSIS-SVD standard register descriptions — see §C.3.2 below.
2. **Visibility class.** Control (writes trigger behaviour), status (reads observe state), data (FIFO ingress/egress), interrupt flag (sticky bit set by peripheral, cleared by firmware), clear-on-read (read has a side-effect). Each class has standard concurrency semantics: control writes are exclusive, status reads can be concurrent, FIFO accesses are atomic at the register granularity but may race with internal counters.
3. **Access path.** Direct memory-mapped load/store, MMIO indirection through a bridge (AHB, AXI-lite), DMA-mediated transfer. The path matters for arbitration semantics — direct MMIO is one-cycle, AHB is multi-beat with potential back-pressure, DMA is asynchronous to firmware execution.

The coupling is itself a contract — in the Document A §3 sense:

- An **assumption** on the SV side: "firmware writes CTRL.go only when STATUS.busy is 0."
- A **guarantee** on the SV side: "peripheral clears STATUS.busy within K cycles of the transaction completing."
- An **assumption** on the C side: "the interrupt service routine runs within N cycles of IRQ rising."
- A **guarantee** on the C side: "firmware acknowledges each pending interrupt before re-enabling the controller."

The discharge graph machinery from Document A §3.x applies unchanged: each clause has an owner (SV or C), each clause is matched against a guarantor (the peer side or the top-level environment), and Tarjan SCC detects circular reasoning before verification runs.

This is also the framing of de Alfaro & Henzinger's *Interface Automata* (ESEC/FSE 2001) applied at the HW/SW boundary: the peripheral's interface is the register-map view; the firmware's interface is the same register-map view from the opposite side; they are *compatible* if some environment (clock, reset, interrupt controller) makes their composition useful.

## C.3 The pipeline

Six stages, with current implementation status anchored to live code:

```
SV extract  ─┐
              ├─► Coupling synthesis ─► Joint composition ─► Verify ─► Report
C extract   ─┘                                                          │
              ▲                                                         │
              │                                                         ▼
            Register-map sidecar                              Interleaved
            (D §D.3 coupling/                              counterexample
             register_maps/)                          ([SW] / [HW] / [BUS])
```

| Stage | Status | Notes |
|---|---|---|
| **SV extract** | ✓ exists | Either RTL pipeline from [Document B](rtl-frontend-unification.md). For codesign, prefer the custom-SV pipeline at protocol granularity unless the property is bit-level. The [`BlackBoxInterface`](../../crates/mununu-core/src/contract/discover.rs) auto-emission lands the SV-side artefact at the same shape this document consumes. |
| **C extract** | ⚠ gap | `mununu-extract`'s `ast` backend at [crates/mununu-extract/src/main.rs](../../crates/mununu-extract/src/main.rs) currently supports TypeScript / Python / Rust. **C is not on the list.** See §C.3.1 below for the tooling assessment. |
| **Register-map sidecar** | ⚠ gap | A small JSON file describing each register: name, base+offset, width, direction, access semantics, mapping to SV signal names *and* C field accessors. This is the coupling spec. Hand-authored from the SoC's datasheet (the same place the firmware engineers got it). See §C.3.2 for the schema sketch. |
| **Coupling synthesis** | ⚠ gap | New step: read both extractions plus the register-map sidecar, produce a unified [`AdapterIR`](../../crates/mununu-core/src/adapter/ir.rs) where each register is a coupled variable with rendezvous labels for read/write access. Properly models the bus arbitration semantics (writes are exclusive; reads can be concurrent on RO registers). |
| **Joint composition** | ✓ exists | The composition engine at [composition/mod.rs](../../crates/mununu-core/src/composition/mod.rs) already handles synchronous + asynchronous + shared-label semantics. SV and C are composed asynchronously with rendezvous on register-access labels. No new composition primitive needed. |
| **Verify + report** | ✓ exists (for non-interleaved traces) | Existing CLI / API. The new bit is that counterexample traces become *interleaved* — the report must show which step came from the SV side, which from C, and what the bus saw. The trace-renderer extension is the new work; the verifier itself is unchanged. |

Three of the six stages are gaps; three are existing capabilities. **The C-extract gap is the largest** by engineering effort, but per §C.7 it can be bypassed initially by hand-authoring the firmware automaton in CTXDSL while the register-map sidecar + coupling synthesis ship.

### C.3.1 C extraction tooling — is tree-sitter industrial-ready?

Short answer: **tree-sitter alone is not industrial-ready for C semantics; it is industrial-ready for C syntax.** This matters because firmware verification needs both.

What `tree-sitter-c` gives you (mature, battle-tested in Neovim, GitHub code search):

- Reliable AST for any well-formed C translation unit.
- Robust error recovery on partial / in-progress code.
- Function signatures, struct layouts, typedefs, enum values — all the *shape* of the code.
- Already has a Rust binding (`tree-sitter-c` on crates.io), fits cleanly into `mununu-extract`'s `ast` backend pattern.

What `tree-sitter-c` does **not** give you:

- **Preprocessor handling.** `#include`, `#define`, `#ifdef`, macro expansion. Real embedded C is half-preprocessor; without it the AST you see is not the AST the compiler sees. Tree-sitter cannot evaluate `#if HAS_DMA == 1` and chooses a fixed branch by tokeniser default.
- **Type resolution / sizeof / pointer arithmetic.** Computing struct field offsets (needed for register-map detection from a `struct UART_TypeDef`) requires the C type system. Tree-sitter sees the syntactic struct definition but not its layout.
- **Volatile / memory ordering.** Firmware correctness often turns on `volatile`, `__atomic_*`, memory barriers. Tree-sitter parses them but cannot reason about them.
- **Linker / section semantics.** `__attribute__((section(".vector_table")))` ties code to hardware addresses. The AST sees the attribute; understanding what it *means* requires target-aware knowledge.

The doc evaluates three options for industrial-grade C extraction:

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **tree-sitter-c + custom preprocessor pass + custom type inference** | All-Rust, no external dep, fits existing `mununu-extract` shape | Re-implementing cpp + sizeof in Rust is years of work; will miss real-world edge cases (compiler-specific extensions, attribute-rich vendor headers) | Not recommended for industrial use. OK for shallow extraction (signatures, struct fields, register-map scanning). |
| **libclang (Clang's C API) via the `clang-sys` crate** | Real compiler frontend; full preprocessing, type info, target-aware. Used by `bindgen`, `rust-analyzer` for C interop, `clang-tidy`. Industrial-grade. | External dependency on a libclang.so / .dylib install. Build-time complication. Slower than tree-sitter (parses + resolves types). | **Recommended primary route.** The same dep is already implicitly present on dev machines (most engineers have Clang installed). Ship `mununu-extract --backend libclang` alongside the existing `ast` backend. |
| **LLVM IR via the existing `llvm` backend** | Already partially implemented in `mununu-extract` per the existing flag list. Sidesteps preprocessing entirely — compile firmware to bitcode and extract from the IR. Bit-exact semantics. | Loses some source-level info (variable names mangled, source lines through DWARF only); requires user to compile firmware first; not great for "give me a quick model from this `.c` file" UX. | **Recommended secondary route.** Best for already-built firmware, when the user has the Makefile in hand. Loses ergonomics for ad-hoc extraction. |

**Doc C's recommendation:** ship `libclang` as the primary "I have C source code and want a model" path; keep `llvm` for "I have a built firmware image"; reserve `tree-sitter-c` for shallow extraction tasks (e.g. scanning headers for the register-map sidecar without committing to full semantic analysis). Document the trade-offs honestly in the user-facing CLI help so the user picks the right backend for their situation.

**Simplification opportunity inside §C.3.1.** Do not gate the rest of Doc C on full C extraction. The §C.7 staging proposal — ship the register-map sidecar format first, allow hand-authored firmware automata — means an industrial user can verify cross-boundary properties *today* with a hand-authored C-side automaton, while libclang integration lands as a follow-up.

### C.3.2 Register-map sidecar format

A small JSON file under `.mununu/coupling/register_maps/<peripheral>.json`, matching Document D §D.3's `.mununu/` directory layout. Schema sketch (illustrative — the exact field names are finalised at implementation time per §C.9):

```json
{
  "peripheral": "UART_LITE",
  "base_address": "0x40010000",
  "registers": [
    {
      "name": "CTRL",
      "offset": 0,
      "width_bits": 32,
      "direction": "RW",
      "visibility_class": "control",
      "access_path": "mmio_direct",
      "fields": [
        { "name": "tx_start", "bits": [0, 0],
          "sv_signal": "uart_inst.ctrl_reg[0]",
          "c_accessor": "UART->CTRL.bit.tx_start" },
        { "name": "enable", "bits": [1, 1],
          "sv_signal": "uart_inst.ctrl_reg[1]",
          "c_accessor": "UART->CTRL.bit.enable" }
      ]
    },
    {
      "name": "STATUS",
      "offset": 4,
      "width_bits": 32,
      "direction": "RO",
      "visibility_class": "status",
      "fields": [
        { "name": "tx_busy", "bits": [0, 0],
          "sv_signal": "uart_inst.tx_busy",
          "c_accessor": "UART->STATUS.bit.tx_busy" },
        { "name": "rx_ready", "bits": [1, 1],
          "sv_signal": "uart_inst.rx_ready",
          "c_accessor": "UART->STATUS.bit.rx_ready" }
      ]
    }
  ]
}
```

**Reuse over invent.** Per §C.8, the format should be importable from IP-XACT (IEEE 1685), SystemRDL, or CMSIS-SVD where the user already has one of those. Mununu carries a small superset (the `sv_signal` + `c_accessor` + access_path fields) on top of the existing vocabulary; conversion in either direction is a mechanical mapping.

## C.4 Worked example — UART driver + UART IP

A canonical codesign example. Picks the simplest interesting case so the conceptual machinery is exercisable end-to-end before the implementation plan ramps up.

**SV side.** UART peripheral with three registers:
- `UART_CTRL` (RW): bits `enable`, `tx_start`.
- `UART_STATUS` (RO): bits `tx_busy`, `rx_ready`.
- `UART_DATA` (W for TX, R for RX): 8-bit data word.

Extract via custom-SV pipeline → a 5-state automaton (`idle`, `tx_load`, `tx_send`, `rx_recv`, `rx_ready`) with labels per register access.

**C side.** Firmware function:

```c
void uart_send(uint8_t byte) {
    while (UART->STATUS.bit.tx_busy)
        ; /* poll */
    UART->DATA = byte;
    UART->CTRL.bit.tx_start = 1;
}
```

Extract via the proposed libclang-backed C support → a 4-state automaton (`poll_busy`, `write_data`, `set_start`, `return`) with labels per register access.

**Register-map sidecar.** The §C.3.2 JSON, naming each register and mapping `uart_inst.ctrl_reg[0]` ↔ `UART->CTRL.bit.tx_start`, etc.

**Coupling synthesis.** Joint IR with rendezvous labels: `rd_status_busy`, `wr_data`, `wr_ctrl_tx_start`. SV transitions that *produce* the status signal emit `rd_status_busy` as an `Uncontrollable` label (the firmware reads it — Document A §4 controllability rule); SV transitions that *read* CTRL.tx_start emit `wr_ctrl_tx_start` as the response.

**Verify.** Property: "firmware never writes UART_DATA while peripheral is mid-transmission" → mu-calculus `AG (wr_data → !tx_busy)`. Run the joint model.

**Report.** If the property fails, the trace is interleaved:

```
[SW]  poll_busy returns 0      (race window opens)
[HW]  tx_busy rises             (peripheral starts internal tick)
[SW]  write_data (UART_DATA)
[HW]  FAIL — write during tx_busy
```

This trace would be invisible to either side in isolation: the SW-side trace alone shows a clean write; the HW-side trace alone shows a busy line; the cross-boundary product reveals the race.

## C.5 Soundness considerations specific to codesign

Three places where the standard mununu soundness rules need codesign-specific extensions:

- **Bus arbitration is a real source of non-determinism.** The coupling synthesis must respect this — two parallel writes from firmware and peripheral can interleave. Modelling them as synchronous (one-step rendezvous) is **unsound for properties about racy access**. Recommend asynchronous composition with explicit arbitration labels. Match the existing `composition/mod.rs` async path.
- **Interrupt latency is an environment assumption.** Firmware-side properties typically rely on "interrupt service routine runs within N cycles of IRQ rising." This is a contract on the surrounding system (interrupt controller, CPU pipeline). Treat as a **top-level assumption** following Document A §3 stage-4 HITL approval. The contract sits at the *top* of the discharge graph; no peer guarantees it.
- **Memory model for C extraction.** Embedded C is often not sequentially consistent (compiler reordering, weakly-ordered AHB). The C extractor **must declare its memory model**; properties about volatile / barrier-free access patterns require either an SC abstraction (under-approx → unsound for safety) or an explicit weak-memory composition. Recommend **SC as default with explicit warning**, since most embedded firmware code intends SC but doesn't always achieve it. The same `// SOUNDNESS:` comment discipline from `CLAUDE.md` applies — every memory-model choice must be documented at the extraction site.

### C.5b C-extraction correctness scope

The C extractor (Task C5) ships in slices. Each slice extends what the AST-level pattern matcher can faithfully represent without claiming formal equivalence to the C source. The bounds of what slices *can* and *cannot* prove, and the principled alternative (LLVM-IR / CFG / predicate abstraction) the project would switch to if AST pattern matching proved insufficient, live in [`docs/design/c-extraction-correctness-scope.md`](c-extraction-correctness-scope.md). Every PR that adds a slice beyond 2.c must re-read that document and document the trigger justifying the new slice; if no trigger fires the slice is paused and the principled-lift work is scoped out as a separate milestone.

## C.6 Industrial prioritisation within Document C

Within HW/SW codesign, sub-cases ranked by tractability and industrial reach:

1. **UART / SPI / I²C driver + IP.** Smallest, best-documented register maps; perfect starter use case. The §C.4 worked example.
2. **Interrupt-driven peripheral + ISR.** Adds the interrupt-controller contract — one new top-level assumption (latency bound) and one new SV/C label (IRQ rising). Common in safety-critical firmware (medical infusion controllers, automotive ECUs).
3. **DMA controller + descriptor ring.** Adds shared-memory semantics; substantially harder because the descriptor ring is *not* a single register but a structured memory region with its own coherence story. Future.
4. **Multi-master fabric (e.g. AXI + AHB bridge + DMA).** Adds bus-level arbitration with multiple masters competing for the same slave. Future-future.

Phase-1 implementation should target sub-case 1, validate the coupling synthesis with a published example (an open-source MCU SDK driver — STM32 HAL UART, lowRISC OpenTitan UART), then extend.

## C.7 Simplification opportunity — ship the connecting tissue first

The biggest leverage move: **define the register-map sidecar format precisely, ship it, and let users hand-author register maps even before C extraction lands**. With a hand-authored coupling spec, even a fully hand-authored CTXDSL firmware automaton lets a user verify cross-boundary properties *today*, using machinery mununu already has.

The C extraction work is then a separate, independently valuable, follow-up. This staging matches Document A §3's "discharge-first" recommendation: ship the **connecting tissue** before the **automatic extraction**.

Concretely, the M1 slice is:

1. Register-map sidecar format spec'd and validated (one or two example sidecars checked in).
2. Coupling-synthesis library function that takes `(BlackBoxInterface_sv, hand_authored_ctxdsl_firmware, register_map.json)` and produces a unified CTXDSL composition with rendezvous labels.
3. The §C.4 UART worked example, end-to-end, with **hand-authored** firmware CTXDSL.
4. Interleaved counterexample reporter — minimum viable version that tags each trace step with `[SW]` / `[HW]` / `[BUS]`.

Steps 1–4 ship without any C extraction work. Steps 5–6 (libclang-backed C extraction; LLVM-IR backend polish) are independently scoped follow-ups.

## C.8 Open questions

These are deferred for the implementation phase, not yet resolved:

- **Q1 — Where does the register-map sidecar live?** Default: this repo under `.mununu/coupling/register_maps/`, matching Document D §D.3. Allow `--register-map <path>` to point to an external file for industrial users with proprietary maps that belong in `mununu-private`.
- **Q2 — How to handle memory-mapped peripherals at addresses computed at runtime?** Driver indirection through a base pointer (e.g. `uart_t *uart = (uart_t *)0x40010000;`) means the C extractor must resolve the symbolic address. Either constant-folding in the extractor or a runtime annotation `@mununu_register_base 0x40010000` per §D.5 grammar. Both should work; pick at implementation time.
- **Q3 — Codesign × CIRCT MLIR.** If the third RTL backend hinted at in `mununu-extract`'s `circt` option lands, the coupling layer must slot in unchanged. This is the strongest argument that the coupling spec must be **frontend-agnostic** — it talks to `AdapterIR`, not to any specific RTL frontend's internals.
- **Q4 — Off-the-shelf register-map formats.** IP-XACT (IEEE 1685-2022), SystemRDL, and CMSIS-SVD are the established candidates. The doc recommends **importing from existing formats** rather than inventing a new one; the §C.3.2 schema sketch is intentionally a superset of the CMSIS-SVD register subset so an `svd → mununu` converter is a one-pass mechanical mapping.
- **Q5 — Property templates for codesign.** The template registry at [crates/mununu-core/src/adapter/templates/](../../crates/mununu-core/src/adapter/templates/) should grow a `codesign` domain with patterns like `no_write_during_busy(REG, BUSY)`, `irq_acknowledged_within(IRQ, ACK, K)`, `reset_isolates_state(STATE)`. Same `template_ref` plumbing as the existing domains; the value is in the patterns, not the machinery.

## C.9 Implementation plan

Tasks for the §C.3 six-stage codesign pipeline. Anchors heavily on Document D (register-map sidecar format, `.mununu/` directory layout) and Document A (controllability + chaotic stubs + HITL review). The C-extract gap (§C.3.1) is its own multi-task sub-plan: libclang integration is the primary route, LLVM-IR backend as secondary, tree-sitter for shallow extraction only.

Sequencing: ship the **hand-authored register-map + hand-authored firmware automaton** path first (per §C.7's simplification), then libclang integration as a follow-up.

### §C.9.0 Scoping pass — required gate before §C.9.1

Same shape as Document A §8.0. Because C is the **last** document in the roadmap, its scoping pass is the highest-risk for drift — A, B, and D have all shipped real code by the time C's implementation begins. Re-verify especially:

- Document D's register-map sidecar schema — has it changed from D's draft? Is the `.mununu/coupling/` directory live, or still deferred from D3?
- Document B's unified controllability classifier — is the API stable? Does it apply per-register cleanly at the HW/SW boundary?
- Document A's HITL review surface — does it expose the right hooks for codesign? Will adding cross-boundary `assumption` / `guarantee` clauses produce reviewable proposals through the existing `mununu contract review` command?
- The custom-SV vs yosys decision — has the per-frontend gap narrowed, or do the two paths still need separate codesign treatment?

Write a scoping-log entry at `.claude/plans/scoping-logs/hw-sw-codesign-extraction-implementation.md`. Record a **GREEN / YELLOW / RED** verdict. Mandatory.

### §C.9.1 Task C1 — register-map sidecar schema (foundation)

**Touches:** new module `crates/mununu-core/src/codesign/register_map.rs`, JSON schema at `tools/register_map_schema.json`, example sidecar under `examples/industrial/codesign_uart/register_map.json`.
**Scope:** the schema from §C.3.2 — a `RegisterMap` struct with peripheral name, base address, list of `Register` entries each carrying name, offset, width, direction, visibility class, access path, and a list of `Field` entries with bit-ranges + `sv_signal` + `c_accessor`. JSON Schema validation. Hand-authored example sidecar.
**Validation:** round-trip serde tests; schema validation against the example. Standalone deliverable: no behaviour changes yet, but the sidecar format is locked in and downstream tasks build against it.

### §C.9.2 Task C2 — coupling synthesis (the connecting tissue)

**Touches:** new module `crates/mununu-core/src/codesign/coupling.rs`, CLI subcommand `mununu codesign couple`.
**Scope:** function `synthesise_coupling(sv_iface: BlackBoxInterface, firmware_ctxdsl: ContextDoc, register_map: RegisterMap) → ContextDoc` that produces a unified CTXDSL composition. Each register read/write in the SV side gets a shared `rd_<reg>` / `wr_<reg>_<field>` label; the firmware side mirrors the labels. Controllability of each label follows Document A §4's port-direction rule applied at the register boundary.
**Validation:** end-to-end test on the §C.4 UART example using a hand-authored firmware CTXDSL (no C extraction needed yet). Property `AG(wr_data → !tx_busy)` evaluates to the expected verdict against the composed model.

### §C.9.3 Task C3 — interleaved counterexample reporter

**Touches:** [crates/mununu-core/src/context/](../../crates/mununu-core/src/context/) (where trace rendering lives), [crates/mununu-core/src/api/](../../crates/mununu-core/src/api/) for the API surface.
**Scope:** when a counterexample / counterstrategy trace crosses the coupling, tag each step `[SW]` / `[HW]` / `[BUS]` based on which side of the composition produced it. Surface as new fields on the existing counterexample API response.
**Validation:** unit test on a hand-crafted failing UART scenario; the trace string contains the expected `[SW] poll_busy returns 0 → [HW] tx_busy rises → [SW] write_data` sequence.

### §C.9.4 Task C4 — `mununu codesign verify` CLI / HTTP / UI

**Touches:** [crates/mununu-cli/src/main.rs](../../crates/mununu-cli/src/main.rs), [crates/mununu-core/src/api/handlers.rs](../../crates/mununu-core/src/api/handlers.rs), [mununu-ui/src/components/contract/](../../../mununu-ui/src/components/contract/).
**Scope:** end-to-end three-surface entry point. CLI: `mununu codesign verify <sv> <c-or-ctxdsl> --register-map <path> --formula <name>`. HTTP: `POST /api/v1/codesign/verify`. UI: new "Codesign" sub-tab in ContractPanel or a new top-level panel.
**Validation:** three-surface parity check via the existing `parity-check` skill.

### §C.9.5 Task C5 — libclang backend for `mununu-extract` (with C-side `@mununu_*` wrappers)

**Touches:** new feature flag `--backend libclang` on [crates/mununu-extract](../../crates/mununu-extract/), depends on `clang-sys` crate. Also extends [crates/mununu-core/src/mununu_annotations/mod.rs](../../crates/mununu-core/src/mununu_annotations/mod.rs) with a C-flavoured wrapper parser.
**Scope:** C-side extraction. Parses C source via libclang, resolves preprocessor + types, produces an `AdapterIR` shape matching the existing TypeScript / Python / Rust paths. Stripped down to firmware-relevant constructs: register access, polling loops, ISR handlers.

**Bundled scope — C language wrappers for the annotation grammar.** Document D §D.5 reserved a tag vocabulary across SV / C / TS / Rust / Python; only the SV wrappers ship today. The C wrappers (`/** @mununu_*  ... */` Doxygen comments on function/struct/typedef declarations) **are not optional for this task** — without them a firmware engineer cannot attach `@mununu_guarantee` to an ISR or `@mununu_assume` to an interrupt-latency contract, and the discovery pipeline cannot lift those into proposed clauses for the HITL review surface (the same flow shipped in PR vscorza/mununu#20). C wrappers therefore land **in the same PR as the libclang backend**, not as a separate follow-up. The other languages (TS / Rust / Python) remain long-tail; ship them only when a real demand signal appears.

**Validation:** the §C.4 example's firmware extracts to a 4-state automaton matching the hand-authored CTXDSL from §C.9.2; a vendor-supplied `/** @mununu_guarantee = "G(start -> eventually done)" */` on the `uart_send` function flows through to a `ProposedClause` in the `mununu contract review` output.
**Out of scope:** full C semantics. Deferred follow-ups: function pointers, recursion, complex pointer arithmetic, vendor-specific compiler extensions. Also out of scope: TS / Rust / Python annotation wrappers — long-tail until requested.

### §C.9.6 Task C6 — IP-XACT / CMSIS-SVD importer

**Touches:** `crates/mununu-core/src/codesign/register_map.rs` (extends C1).
**Scope:** read an existing IP-XACT or CMSIS-SVD register description and emit the mununu `RegisterMap` JSON. One-pass mechanical mapping; the fields that are mununu-specific (`sv_signal`, `c_accessor`) start empty and are filled in by a follow-up annotation pass on the source.
**Validation:** import a real CMSIS-SVD file (e.g. STM32F4 UART) and confirm the resulting `RegisterMap` round-trips through serde and parses against the C1 schema.

### §C.9.7 Sequencing summary

| Task | Slice | Standalone value |
|---|---|---|
| C1 | M1 | Register-map schema locked in; downstream depends on it |
| C2 | M1 | Hand-authored firmware CTXDSL + register map = end-to-end codesign verification today |
| C3 | M1 | Interleaved counterexample reports — the UX win that makes codesign feel different |
| C4 | M1 | Three-surface CLI / HTTP / UI entry point |
| C5 | M2 | Libclang C extraction — replaces the hand-authored firmware step |
| C6 | M2 | IP-XACT / CMSIS-SVD importer — removes the hand-authored register-map step for commercial MCU users |

Tasks C1–C4 are the minimum viable slice — they ship the full codesign workflow with hand-authored firmware automata and hand-authored register maps, exactly the staging §C.7 recommends. Tasks C5–C6 are the automation layer on top.

## C.10 Industrial example

A real safety-critical codesign target. Candidates, in order of approachability:

1. **STM32 HAL UART + UART peripheral RTL.** STM32 HAL is open source; vendor CMSIS-SVD register maps are public. The UART driver is small (~200 lines of C) and easy to model. Properties: "no data loss across consecutive sends," "TX complete interrupt always reaches the handler."
2. **OpenTitan UART + bootstrap firmware.** OpenTitan is fully open-source RTL with a documented register map. Bootstrap firmware exists in their reference design. Properties: "firmware never executes before signature verification completes" (extends the secure_boot_rom example from Document A §9 to include the firmware side).
3. **Open-source pacemaker firmware + RTL pulse generator.** Academic reference designs exist (the "PACEMAKER" challenge problem from various formal-methods conferences). Properties: "no pulse fires within refractory period," "every detected R-wave is acknowledged before the next sense window."

The implementation should target #1 first (smallest scope, most reproducible), then #2 (highest profile open-source SoC, ties to Document A's existing example), then #3 as the medical-safety capstone if scope permits.

Ships as `examples/industrial/codesign_<target>/` with both `.sv` and `.c` (or hand-authored `.ctxdsl`) sources, the `register_map.json` sidecar, a `validate.sh` that reproduces a byte-deterministic transcript, and a README following the same shape as the [secure_boot_rom](../../examples/industrial/secure_boot_rom/), [dual_frontend_soc](../../examples/industrial/dual_frontend_soc/), and [tls_handshake](../../examples/industrial/tls_handshake/) examples.

## C.11 Publication plan

This is the **capstone** publication for the four-document arc — the post should explicitly tie back to A, B, D and pitch the whole stack as one coherent industrial story.

### C.11.1 Substack — "Formal verification across the HW/SW boundary: a worked codesign example"

Target length: 2,500–3,500 words. Structure:

1. **Open with the problem.** Every embedded device is a coupled pair (RTL + firmware) and the properties that bite cross the boundary. Verifying each side in isolation misses them. Mainstream tools (Verdi, ZeBu, Palladium) handle simulation; formal verification across the boundary is rare.
2. **Frame.** Two reactive modules + a coupling. Register-map sidecar as the coupling spec. Anchor to Reactive Modules + Interface Automata.
3. **Walk the §C.4 UART example end-to-end.** Show the SV side, the firmware side, the register-map sidecar, the joint composition, the interleaved counterexample.
4. **Tie back to the prior posts.** This is what A's contract machinery + B's RTL pipelines + D's corpus + annotations were *for*. Show the discharge graph that links firmware-side and HW-side assumptions through the register map.
5. **Honest scope.** What's not in the example (full DMA, multi-master, weak memory). The chaotic-stub default still applies to anything outside the register-map sidecar.
6. **What's next.** Phase 4 governance — propagating the new vocabulary through CLAUDE.md, agent prompts, the `parity-check` skill.

### C.11.2 LinkedIn — executive summary

Target: 150–200 words. Names the audience (embedded engineers, formal-methods practitioners, automotive / aerospace / medical), the one-line proposition ("verify across the HW/SW boundary with assume-guarantee contracts and a register-map sidecar"), and the byte-deterministic reproducibility of the worked example.

### C.11.3 Validation gate

Same four gates as the prior posts (per [Document D §D.10](contract-corpus-and-config.md)):

- [ ] Gate 1 — `validate.sh` exits 0; `git diff transcript.txt` is empty.
- [ ] Gate 2 — every verdict block quoted in `substack.md` matches `transcript.txt` byte-for-byte.
- [ ] Gate 3 — claims-integrity sign-off per CLAUDE.md: no bug-finding claim against any real device; the worked example's chaotic-stub residuals are surfaced honestly; the register-map sidecar is described as a *coupling spec*, not as a verified model of any vendor's silicon.
- [ ] Gate 4 — second reviewer confirms the "what this does not claim" section is not below the fold.

Only proceed to posting after all four gates pass. Capstone post; high stakes for the four-doc arc's credibility.

## C.12 What comes next

→ **Phase 4 — governance update.** Once Document C is implemented, validated, and published, propagate the new vocabulary into [CLAUDE.md](../../CLAUDE.md), agent prompts (`verification-prospector`, `target-executor`, `review-orchestrator`, `soundness-check`, `parity-check`, `domain-adequacy`), and any affected skills. The M5 milestone in the original roadmap.

The four-document arc closes here.

## C.13 Verification — review checklist for this document

Per the same rules as A / B / D — every code reference must resolve, every claim about industrial precedent must cite at least one source, every "what this does NOT claim" line must be testable against the implementation.

1. Re-read against the §C.3 pipeline status table and confirm each ✓ / ⚠ tag matches current `main`.
2. Confirm the §C.3.1 C-extraction trade-off table is honest: tree-sitter does have a Rust binding (it does — `tree-sitter-c` on crates.io), `clang-sys` does work for production tooling (it does — `bindgen` is the proof), the existing `llvm` backend on `mununu-extract` does exist (it does — see the binary's `--backend` flag).
3. Confirm the §C.4 UART example uses only mechanisms named earlier in the doc; no surprise primitives.
4. Confirm §C.5 soundness considerations match the existing [CLAUDE.md soundness rules](../../CLAUDE.md) — over-approx + safety = sound; under-approx + safety = unsound; etc.
5. Cross-check cross-document references resolve: every link to A / B / D is to a real heading; every code reference is to a real path on `main`.
6. Confirm §C.11.3 publication gate matches the gate language used in the prior `publications/README.md` files (no drift).
7. Confirm §C.13 (this checklist) is a `Verification` checklist, not a `Concept:` or `Status: planning` section — this doc is design-stage but the verification rules apply to all design docs that prescribe schema or implementation.
