> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/design/rtl-frontend-unification.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# RTL frontend unification — what can be unified, what must stay separate

> **Status:** Design-stage reasoning. Not shipped architecture.
> **Audience:** Adapter authors choosing where to slot new RTL functionality, and reviewers asking why mununu keeps two SystemVerilog pipelines.
> **Companion documents:** [A — Black-box modules in compositional extraction](black-box-modules.md), [D — Contract corpus and config](contract-corpus-and-config.md), [C — HW/SW codesign extraction](hw-sw-codesign-extraction.md) (design landed; implementation deferred).

## B.1 Why we have two RTL pipelines

Mununu ships two SystemVerilog frontends today. They were built at different times for different reasons and serve different verification audiences. The reasons are worth stating explicitly because the rest of this document only makes sense once "two RTL pipelines" stops looking like an accident.

**Custom SV pipeline** ([crates/mununu-core/src/adapter/systemverilog/](../../crates/mununu-core/src/adapter/systemverilog/)). Started as the only path. Parses a tightly-scoped subset of SystemVerilog directly into mununu's IR, builds explicit-state Kripke structures from register valuations using SMT-backed symbolic combinational evaluation. Designed to verify protocol-level invariants — handshake correctness, request-grant fairness, FIFO occupancy bounds — where bit-exact behaviour is irrelevant and a symbolic, register-as-enum abstraction yields tractable state spaces. The pipeline's strength is its precision tier: signals declared as enums or `BoundedCounter` per [annotation.rs](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs) collapse the state space dramatically while preserving the protocol behaviour the user cares about.

**Yosys frontend** ([crates/mununu-core/src/adapter/yosys/](../../crates/mununu-core/src/adapter/yosys/) → BTOR2 → [crates/mununu-core/src/adapter/btor2/](../../crates/mununu-core/src/adapter/btor2/)). Added to handle the SystemVerilog the custom SV path cannot — classes, interfaces, generate blocks, packages, parameterised modules, the full SV-2017 surface. Yosys is invoked as a subprocess; its script reads the design, runs `prep -top`, `proc`, `flatten`, `async2sync`, and `chformal -lower`, then emits BTOR2 ([yosys/mod.rs:242](../../crates/mununu-core/src/adapter/yosys/mod.rs#L242)). The BTOR2 reader bit-blasts the resulting netlist into an explicit-state CLTS. Designed to verify bit-exact properties — overflow, sign-extension, post-synthesis correctness — against arbitrary user-written SV.

Two pipelines, two precision tiers, two audiences. Document B's argument is that **this is the right structure**, and most of the design pressure should go into making the seams between the two pipelines (the IR they share, the composition semantics they target, the controllability rule they apply) crisp, while leaving their internal extraction techniques free to differ.

## B.2 The unification principle

Borrowing the canonical multi-frontend pattern from CIRCT ([circt.llvm.org](https://circt.llvm.org/), the LLVM-based hardware compiler infrastructure): **unify the seams, leave the cores free**.

Specifically:

- The *interface* between a frontend and the rest of mununu — the `AdapterIR` shape, the `CompositionSpec` it produces, the `PropertyRole` contract, the controllability classification rule — is a single shape both pipelines must produce.
- The *semantic boundary rules* — Document A §4's port-direction-driven controllability classification, the chaotic-stub default for black-box submodules, A/G role tagging on properties — follow one set of definitions across both pipelines.
- The *internal extraction technique* — how a pipeline gets from SystemVerilog text to `AdapterIR` — is free to differ. Two precision tiers serving two audiences is a feature, not a bug.

This is the same separation Reactive Modules (Alur & Henzinger, *Reactive Modules*, FMSD 15(1), 1999) draws between the *module language* and the *underlying semantics*: different syntaxes can target the same compositional model. It is also the spirit of Interface Automata (de Alfaro & Henzinger, *Interface Automata*, ESEC/FSE 2001): components are characterised by their interface, not by their implementation strategy.

Mununu's two pipelines started as monoliths and the principle has to be retrofitted. That is what §B.3 and §B.4 do.

## B.3 What can be unified (and should be)

These are seam-level alignments — refactors, not rewrites. The table below is the M2.b-impl scope.

| Item | Current state | Unified state | Why |
|---|---|---|---|
| Output IR | ✓ both produce `AdapterIR` | (no change) | Already aligned. |
| CTXDSL emit | ✓ shared via [adapter/emit.rs](../../crates/mununu-core/src/adapter/emit.rs) | (no change) | Already aligned. |
| `PropertyRole` tagging | ✓ shared enum at [adapter/ir.rs:211](../../crates/mununu-core/src/adapter/ir.rs#L211); ⚠ yosys path mostly produces `Standalone` because BTOR2 `bad`/`constraint` → role mapping is partial | Both pipelines preserve A/G distinction faithfully | The contract subsystem (Document A §3) needs both roles round-tripped. |
| Controllability at top-level inputs | ✓ custom-SV uses port direction at [kripke.rs:1030](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L1030); ⚠ BTOR2 defaults all inputs `Uncontrollable` + CLI override list ([bit_blast.rs:280-293](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L280-L293)) | Both pipelines call `controllability::classify_label` ([controllability.rs](../../crates/mununu-core/src/controllability.rs), shipped in Document A task A4); inputs of the top-of-scope module are `Uncontrollable`, outputs `Controllable`, with the override list as escape hatch only | The shared rule already exists; the BTOR2 path needs to preserve port directions through yosys and feed them through. Today the port directions are *in* the SV AST yosys parsed — they are thrown away at the BTOR2 emission boundary. |
| Black-box submodule handling | ⚠ custom-SV: silently ignores instantiations not in sidecar; yosys: `flatten` runs unconditionally over `(* blackbox *)` bodies, producing degenerate Kripke | Both: detect black-box attribute → emit as a separate IR component with a chaotic stub; **auto-emit a `BlackBoxInterface.json` sidecar** alongside the CTXDSL output so the user can run `mununu contract discover` against the auto-generated description rather than hand-authoring it. (This is the "stage 1" integration between the contract subsystem and the extraction pipeline.) | Yosys natively respects `(* blackbox *)` and `keep_hierarchy` — the unification is making the BTOR2 emission preserve the boundary instead of `flatten`-ing it away, and emitting the interface sidecar so the rest of the contract surface picks it up. |
| Composition spec ([`ConnectionSpec` at annotation.rs:302](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L302), sync vs async) | ✓ custom-SV emits it via [`generate_multi_sidecar` at annotation.rs:1058](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L1058); ✗ yosys does not (everything is one flat circuit) | Yosys produces N+K IR components when there are K black-box submodules, plus a top-level composition spec, just like custom SV. | Same backend (mununu's `composition::compose`) → same composition primitive. |
| Counterexample / counterstrategy machinery | ✓ already CLTS-level, frontend-agnostic | (no change) | Already aligned. |
| Clock / reset *labelling* (not semantics) | ⚠ inconsistent: BTOR2 hard-codes "clocks never controllable" ([bit_blast.rs:280-293](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L280-L293)); custom-SV uses annotations | Both: clocks/resets get a uniform label class so formulas can be written portably across the two frontends. | Otherwise the same formula targets one pipeline but not the other — the divergence is invisible to the user but breaks portability. |

**Stage-1 integration with the contract subsystem.** The fifth row above is the load-bearing addition for M2. When either pipeline encounters a black-box module, it now:

1. Emits a `<module>.interface.json` matching the `BlackBoxInterface` schema introduced in Document A task A5 ([crates/mununu-core/src/contract/discover.rs](../../crates/mununu-core/src/contract/discover.rs)) alongside its CTXDSL output.
2. Emits a `<module>.gap_report.json` matching `GapMarkerReport`, prefilled with at minimum an `OutputSequencing` gap covering the module's outputs.

The user no longer hand-writes the JSON. `mununu contract discover` and `mununu contract gaps` consume the auto-emitted files as before. Stage 2 (source-comment annotations populating the discovered contract — task A6) and stage 3 (CTXDSL grammar extension for inline `contract { ... }` blocks) are deferred to M3 and beyond per [Document A §11](black-box-modules.md#11-what-comes-next).

## B.4 What should NOT be unified (and why)

These are core-level divergences that exist for principled reasons. Future contributors will see "two pipelines for the same thing" and want to collapse them; this table is the prevention.

| Item | Custom SV | Yosys / BTOR2 | Why divergence is correct |
|---|---|---|---|
| **Abstraction tier** | Symbolic, register-as-enum, `BoundedCounter` | Gate-level, bit-exact | Different verification questions need different precisions. Forcing one tier loses one audience. Protocol invariants live at the enum tier; overflow / sign-extension bugs live at the bit tier. |
| **Parser** | Tree-sitter + bespoke parser, partial SV coverage | Yosys's mature SV frontend, full SV-2017 support | Maintaining a competitive SV parser is a multi-year investment yosys has already paid. Replicating it in Rust would burn engineering for a tiny gain in homogeneity. |
| **Clock-domain semantics** | Preserved; user can write properties about clock edges | Rewritten by `async2sync`; clocks are abstracted away | Clock-domain analysis requires the original semantics; bit-exact post-synthesis verification benefits from the rewrite. They are different verification problems. |
| **Bit-width handling** | Bounded; signals declared as enums or `BoundedCounter` per [annotation.rs](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs) | Native arbitrary width via BTOR2 | Bit-exact properties (overflow, sign-extension bugs) require full width; protocol properties don't. |
| **SV language coverage** (classes, interfaces, generate, `package`, parametrisation) | Limited, opinionated | Full | Yosys is the bridge for arbitrary SV; custom SV is the bridge for SV-written-the-mununu-way. |
| **External tool dependency** | None — pure Rust | Subprocess to yosys (must be installed) | Custom SV is the zero-dep path for embedding mununu in tools that cannot ship yosys (e.g. WASM builds, restricted CI). |
| **Performance profile** | High variance, dominated by SMT calls; great when the abstraction holds, poor when it doesn't | Predictable, dominated by bit-blast width | Two profiles for two access patterns; the user picks. |

The principle behind the table: **two pipelines, one IR; two precision tiers, one set of semantic boundary rules.** A reviewer who later wants to collapse this to a single pipeline must explicitly argue against the trade-offs in this table.

## B.5 Migration / sequencing recommendation

A short ordered list of refactors implied by the §B.3 table. **Not a commitment** — just the natural sequencing if and when someone takes this on. The implementation plan in §B.7 turns each row into a concrete task.

1. Plumb port-direction-derived controllability through yosys → BTOR2 (replace the "all uncontrollable + CLI override" default).
2. Add yosys-frontend respect for `(* blackbox *)`: emit per-blackbox stub component instead of flattening through.
3. Auto-emit `BlackBoxInterface.json` + `GapMarkerReport.json` sidecars from both pipelines when they encounter a black-box module — the stage-1 contract-subsystem integration.
4. Lift composition spec generation out of the custom-SV path into a shared helper both frontends call.
5. Reconcile clock/reset label class.
6. Make BTOR2 `bad`/`constraint` round-trip to `PropertyRole::{Guarantee, Assumption}` faithfully.

Item (3) is the critical-path item for connecting the contract subsystem to real extraction. Items (1) and (2) are prerequisites for (3) on the yosys side. Items (4)–(6) are polish that can land independently.

## B.6 Worked example

A small MCU SoC with a UART peripheral. The custom-SV pipeline extracts the UART FSM at protocol granularity (3-state idle/tx/rx automaton). The yosys pipeline extracts the same UART at bit-level (shift register, baud divider, RX/TX state machines after `flatten`). Both pipelines target the same `AdapterIR` shape, both apply the same controllability classification (TX line: `Controllable` from MCU side; RX line: `Uncontrollable`), both produce the same `ConnectionSpec` for the peripheral's interconnect with the rest of the SoC.

The verdicts are different — by design.

- Custom-SV: "no protocol deadlock" — a 3-state liveness question over a 3-state automaton.
- Yosys: "no bit-level under/overrun" — a question over the shift register's full 8-bit state.

Both verdicts are valid. Running both pipelines against the same design gives complementary evidence: the protocol is well-formed *and* the bit-level implementation does not introduce overflow paths. Document B's argument is that this should be supported as a deliberate workflow, not as an accident.

The §B.8 industrial example formalises this — a single design verified by both pipelines under their respective abstraction tiers, with the verdicts compared.

## B.7 Implementation plan

The §B.5 sequencing — **(1), (2), (3), (4)–(6)** — translates into the following concrete work items.

**Before any task in §B.7.1–§B.7.6 starts, the §B.7.0 scoping pass must be completed.** Same gate as Document A §8.0.

### B.7.0 Scoping pass — required gate before implementation

Same shape as [Document A §8.0](black-box-modules.md#80-scoping-pass--required-gate-before-implementation), specialised to Document B's surface.

**Inputs**: this document; the current state of `crates/mununu-core/src/adapter/{systemverilog,yosys,btor2}/`; `crates/mununu-core/src/contract/discover.rs` (consumes the sidecars this plan emits); the shared `controllability` module (consumed by B.7.1).

**Checklist** (each item produces a written note in the scoping log):

1. **Re-read §B.1–§B.6** end-to-end with fresh eyes. Confirm the conceptual frame still holds.
2. **Re-verify every code reference** resolves: the yosys `flatten` line, the BTOR2 controllability defaults, the SV `classify_signal` function, the `BlackBoxInterface` schema in the contract module.
3. **Re-verify cross-doc dependencies**: Document A's contract subsystem must be on `main` (it is — A1, A2, A3, A4, A5, A8 are merged via PR #10).
4. **Re-verify sequencing**: §B.5's order should still be optimal given current pipeline state.
5. **Re-verify cost estimates**: are any of the tasks below off by >3× from the rough sizing here?
6. **Scoping log entry** at `.claude/plans/scoping-logs/rtl-frontend-unification-implementation.md`.
7. **Verdict** GREEN / YELLOW / RED.

**Repetition rule**: re-run if implementation pauses >2 weeks at any point during §B.7.1–§B.7.6.

### B.7.1 Task B1 — yosys port-direction preservation
**Touches:** [crates/mununu-core/src/adapter/yosys/mod.rs](../../crates/mununu-core/src/adapter/yosys/mod.rs), [crates/mununu-core/src/adapter/btor2/bit_blast.rs](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs).
**Scope:** before the yosys script runs `flatten`, snapshot the port direction of every top-level input. Yosys exposes this via the `info` command on the elaborated hierarchy; capture the output and feed it through the BTOR2 emitter as a sidecar JSON. The BTOR2 reader consumes the sidecar and calls `controllability::classify_label` for each input. CLI override list (`--controllable-inputs`) stays as escape hatch.
**Validation:** regression test against an existing BTOR2 example with mixed input/output. The auto-classified controllability matches the custom-SV verdict on the same design.

### B.7.2 Task B2 — yosys black-box detection
**Touches:** [crates/mununu-core/src/adapter/yosys/mod.rs](../../crates/mununu-core/src/adapter/yosys/mod.rs).
**Scope:** detect `(* blackbox *)` and `keep_hierarchy` attributes on submodules before `flatten` runs. Marked modules are extracted separately (port list snapshot + module name + source location) and excluded from the flatten pass. The custom-SV path already handles this via the multi-module sidecar (`crates/mununu-core/src/adapter/systemverilog/annotation.rs`); the change is to share the detection logic so both frontends agree on what counts as a black box.
**Validation:** test against an SoC fragment with one `(* blackbox *)` module — yosys output now contains two IR components (the visible interior + the chaotic stub) instead of one degenerate Kripke.

### B.7.3 Task B3 — adapter-side `BlackBoxInterface` + `GapMarkerReport` sidecar emission
**Touches:** [crates/mununu-core/src/adapter/systemverilog/](../../crates/mununu-core/src/adapter/systemverilog/), [crates/mununu-core/src/adapter/yosys/](../../crates/mununu-core/src/adapter/yosys/), [crates/mununu-core/src/adapter/emit.rs](../../crates/mununu-core/src/adapter/emit.rs).
**Scope:** when either pipeline encounters a black-box module, emit two JSON sidecars next to the CTXDSL output:
- `<module>.interface.json` matching the `BlackBoxInterface` schema from [contract/discover.rs](../../crates/mununu-core/src/contract/discover.rs).
- `<module>.gap_report.json` matching `GapMarkerReport` from [contract/gap.rs](../../crates/mununu-core/src/contract/gap.rs), prefilled with at minimum an `OutputSequencing` gap.

Both sidecars use the same paths, names, and JSON shapes as the §A5 phase-1 discovery command outputs, so downstream tools (`mununu contract discover`, `mununu contract gaps --strict-contracts`, the UI's Discover panel) consume the auto-generated files transparently.

The diagnostic warning (one `tracing::warn!` per gap) fires at extraction time, not at a separate `discover` command. **This is the moment the contract subsystem stops being a separate JSON workflow and becomes an automatic byproduct of extraction.**

**Validation:** end-to-end test that running the SV adapter on a design with a `(* blackbox *)` module produces:
- One CTXDSL output (the visible interior).
- One `<bbname>.interface.json` (the auto-discovered interface).
- One `<bbname>.gap_report.json` (the auto-generated gap report).
- One `WARN contract gap detected — chaotic stub default in effect` per gap, with module / kind / labels / soundness fields.

### B.7.4 Task B4 — shared composition-spec helper

> **Post-M2 reassessment (2026-05-13): lost most of its leverage.** The dual-frontend SoC example shipped in M2.c demonstrates that both pipelines emit structurally-identical `BlackBoxInterface.json` (verified by `jq` normalisation in `examples/industrial/dual_frontend_soc/validate.sh`). The unification happened via **convention** — both paths call `contract::discover::build_blackbox_sidecars()` — rather than via an explicit shared composition-spec helper. Lifting `ConnectionSpec` into a separate helper today would be **code-reduction without behaviour change**. Worth doing only if a *third* RTL frontend lands (e.g. CIRCT via [crates/mununu-extract](../../crates/mununu-extract/)) and the duplication becomes painful. Until then, B4 is descoped; the two paths converge on the same output through shared lower-level helpers (`build_blackbox_sidecars`) rather than a shared composition primitive.

**Touches:** [crates/mununu-core/src/adapter/](../../crates/mununu-core/src/adapter/) (new `compose.rs` or extend `emit.rs`).
**Scope:** lift the `ConnectionSpec` data type ([annotation.rs:302](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L302)) + composition-emission code (the body of [`generate_multi_sidecar` at annotation.rs:1058](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L1058)) out of the custom-SV path into a shared helper both frontends call. The custom-SV port-binding logic becomes the canonical implementation; yosys's new black-box-aware path calls into it.
**Validation:** the dual-frontend SoC example (§B.8) produces structurally identical composition specs from both frontends (same composition name, same member list, same shared-label set).

### B.7.5 Task B5 — clock / reset label class reconciliation
**Touches:** [crates/mununu-core/src/adapter/yosys/](../../crates/mununu-core/src/adapter/yosys/), [crates/mununu-core/src/adapter/btor2/](../../crates/mununu-core/src/adapter/btor2/), [crates/mununu-core/src/clts/](../../crates/mununu-core/src/clts/).
**Scope:** define a uniform label class for clock/reset signals across both pipelines. Two options:
- (a) Use `LabelControllability::Internal` for clocks/resets uniformly; mu-calculus modal guards can reference them but they are not adversarial.
- (b) Introduce a new `LabelControllability::Clock` variant.

Option (a) is the lower-cost route; option (b) is cleaner but touches the CLTS enum and ripples through every adapter. Recommend (a) unless the scoping pass surfaces a property class that needs (b).
**Validation:** the same property formula references `clk`/`reset` consistently across both frontends.

### B.7.6 Task B6 — `bad`/`constraint` → `PropertyRole` round-trip
**Touches:** [crates/mununu-core/src/adapter/btor2/](../../crates/mununu-core/src/adapter/btor2/).
**Scope:** today the BTOR2 reader produces properties mostly as `PropertyRole::Standalone`. Yosys's `chformal -lower` lowers SVA `assert` → BTOR2 `bad` lines and SVA `assume` → BTOR2 `constraint` lines. Map them faithfully: `bad` → `PropertyRole::Guarantee`, `constraint` → `PropertyRole::Assumption`.
**Validation:** an SVA-annotated design with `assert` and `assume` properties round-trips through yosys → BTOR2 → mununu and the property roles survive.

### B.7.7 Sequencing summary

The recommended landing order is **B1 → B2 → B3 → B4 → B5 → B6**. Each row delivers value standalone:

| Task | Standalone value |
|---|---|
| B1 | yosys path stops throwing away port directions; controllability is principled in both pipelines |
| B2 | yosys path stops obliterating module boundaries on `(* blackbox *)` designs |
| B3 | the contract subsystem stops requiring hand-authored JSON for modules the extractor already saw — **the stage-1 integration this document promises** |
| B4 | both frontends produce the same composition shape for the same design |
| B5 | properties become portable across pipelines |
| B6 | A/G roles round-trip through yosys |

B1–B3 are the **minimum viable slice** — if work pauses after B3, mununu has gained the unified controllability rule, real black-box handling on the yosys side, and the contract-subsystem integration. B4–B6 are polish that can ship later.

## B.8 Industrial example — dual-frontend SoC verification

The example exercises both pipelines against the same RTL and demonstrates the §B.4 "two precision tiers" principle in action.

### B.8.1 Why this example

- **Realistic.** A small SoC (a RISC-V Ibex core, a memory controller, and a UART peripheral) is the kind of artefact that lives in every embedded-systems undergraduate course and every open-source MCU project.
- **Black-box-essential.** The memory controller is `(* blackbox *)` — a closed-IP DDR3 PHY, modelled as a chaotic stub via the new stage-1 sidecar emission.
- **Two pipelines, one design.** The Ibex + UART side is verified at protocol granularity by custom-SV (verifying "the UART never drops a byte from the CPU"). The same design is verified at bit-level by yosys → BTOR2 (verifying "the UART shift register never overflows"). Both verdicts hold; together they constitute complementary evidence.

### B.8.2 Components

```
┌──────────────────────────────────────────┐
│ Open-source SoC (verifiable by both      │
│ pipelines)                               │
│  ├─ Ibex RV32 core                       │
│  └─ UART peripheral (TX/RX shift regs)   │
└──────────────────────────────────────────┘
                  │
                  ▼
┌──────────────────────────────────────────┐
│ DDR3 PHY (closed-IP black box, V2)       │
│  (* blackbox *)                          │
└──────────────────────────────────────────┘
```

### B.8.3 What the example demonstrates

| Concept | Demonstration |
|---|---|
| §B.3 row 5 — black-box detection + sidecar emission | The yosys pipeline encounters the `(* blackbox *)` DDR3 PHY and emits `ddr3_phy.interface.json` + `ddr3_phy.gap_report.json` without user intervention. |
| §B.3 row 4 — unified controllability rule | The UART's RX/TX pins are classified `Uncontrollable`/`Controllable` identically by both pipelines (port direction at the top-module boundary, via the shared `controllability::classify_label`). |
| §B.4 — two precision tiers | Custom-SV verifies a protocol property; yosys/BTOR2 verifies a bit-level property. Both pass; the verdicts complement each other. |
| §B.5 — composition spec is shared | Both pipelines produce the same `CompositionSpec` (sync composition with members `Ibex`, `UART`, `DDR3_PHY_stub`). |

### B.8.4 Concrete validation script

The example ships as `examples/industrial/dual_frontend_soc/` with:

```
examples/industrial/dual_frontend_soc/
├── README.md
├── soc.sv                              # the open SoC source
├── ddr3_phy_stub.sv                    # the (* blackbox *) declaration
├── ibex_uart.mununu.json               # multi-module sidecar for custom-SV
├── soc.ctxdsl                          # mununu's custom-SV extraction output
├── soc.btor2                           # yosys output (gitignored, regenerated)
├── ddr3_phy.interface.json             # auto-emitted by extraction (sidecar)
├── ddr3_phy.gap_report.json            # auto-emitted by extraction (sidecar)
├── properties.ctxdsl                   # protocol + bit-level properties
└── validate.sh                         # runs both pipelines, compares verdicts
```

`validate.sh` does:

1. Run the custom-SV extractor on `soc.sv` + `ibex_uart.mununu.json` → produces `soc.ctxdsl` + auto-emits `ddr3_phy.{interface,gap_report}.json`.
2. Run the yosys frontend on `soc.sv` → produces `soc.btor2` + auto-emits the same sidecars (the JSON shape is the same regardless of pipeline).
3. Run `mununu context eval` with the protocol property against the custom-SV output.
4. Run `mununu context eval` with the bit-level property against the yosys-derived CLTS.
5. Cross-check: the auto-emitted `ddr3_phy.interface.json` from both pipelines is byte-identical.
6. Emit a deterministic transcript.

The byte-identical-sidecar check (step 5) is the load-bearing verification: it proves that the two pipelines agree on what the black-box module *is* even if they disagree on the precision tier of the rest.

### B.8.5 What the example does NOT claim

Per the [CLAUDE.md claims-integrity rules](../../CLAUDE.md):

- It does **not** claim mununu found a bug in any commercial DDR3 PHY or Ibex core.
- It does **not** claim the bit-level UART property the yosys path verifies is the property a real designer would write — it is illustrative.
- It does **not** demonstrate vendor `@mununu_guarantee` annotations on the DDR3 PHY (those are M3, task A6).
- It does **not** prove that any real SoC using a closed DDR3 PHY is secure or correct. The proof is conditional on the chaotic-stub contract; vendor contracts authored later tighten this.

## B.9 Publication plan

Two derivative artefacts publish the result after the §B.8 transcript is reproducible.

### B.9.1 Substack — "Two RTL frontends, one IR: what stays separate and why"

**Audience:** formal-methods practitioners, hardware verification engineers, EDA tool authors.

**Structure:**
1. Why mununu has two RTL pipelines (it is deliberate, not accidental).
2. The unification principle — seams unified, cores divergent (CIRCT parallel).
3. The §B.3 / §B.4 tables walked through with concrete examples.
4. The stage-1 contract integration: the moment extraction stops being separate from contracts.
5. Walking the dual-frontend SoC example end-to-end, with the actual transcript.
6. Honest caveats from §B.8.5.
7. Pointer to Document D as the next milestone.

**Length target:** 2500–3500 words. One transcript block, two diagrams (the SoC architecture + the dual-pipeline flow), no marketing language.

### B.9.2 LinkedIn — "Why mununu has two SystemVerilog pipelines (and won't merge them)"

**Audience:** semiconductor / formal verification leadership, technical decision makers.

**Structure:**
- One-sentence framing of the dual-pipeline question.
- One-sentence answer: different precision tiers, one set of semantic boundary rules.
- Two-sentence summary of the dual-frontend SoC example.
- Link to the Substack deep dive and the example directory.

**Length target:** 150–200 words.

### B.9.3 Validation gate (same shape as Document A §10.3)

Before either draft posts publicly, all four checks must pass:

1. `./examples/industrial/dual_frontend_soc/validate.sh` exits 0 against the pinned commit.
2. The transcript referenced in the Substack post matches the transcript the script produces (verdict lines byte-for-byte).
3. The claims integrity checklist is signed off — no overclaim about real silicon, no merging of the precision tiers in the writing, all abstractions named.
4. A second reviewer (human or `review-orchestrator` agent) confirms §B.8.5 caveats are not buried.

Drafts are not written before §B.8 is reproducible.

## B.10 What comes next

When this document is marked **implemented** (tasks B1–B6 landed), **validated** (the §B.8 transcript is reproducible), and **published** (the §B.9 posts are live), the next document to tackle is:

→ **[Document D — Contract corpus and config](contract-corpus-and-config.md)** (deferred) and its accompanying implementation plan.

Document D picks up the relocated A6 (corpus-driven discovery phase 2) and A7 (HITL stage-4 UX) from Document A, plus its own corpus + sidecar + L\* surface scope. It is the next document because Document C (HW/SW codesign) depends on D's register-map sidecar format.

The full roadmap order: **A → B → D → C → governance update**. See the planning file at `.claude/plans/i-want-you-to-distributed-orbit.md` for the milestone breakdown.

## B.11 Open questions

- **Is the third pipeline coming?** The `mununu-extract` crate has a `circt` backend ([crates/mununu-extract/src/main.rs](../../crates/mununu-extract/src/main.rs)) for CIRCT MLIR. If a third RTL frontend ever materialises, the §B.2 principle generalises — the IR is the seam for *all* RTL frontends, not just the two we have today. The §B.3 / §B.4 tables would gain a column; the recommendation should still hold.
- **Should the IR carry a `frontend_hint` field** so consumers (e.g. counterexample explainers, UI) can tailor presentation to symbolic vs. gate-level? Or is that an anti-pattern that would leak the abstraction tier into downstream code that should not care?
- **Should there be a way to run both pipelines on the same design and cross-check the verdicts** routinely, beyond the §B.8 example? Where the abstractions overlap, agreement is reassuring; disagreement is a diagnostic.
- **What about the BTOR2 reader's CLI override list?** Once B1 lands, the `--controllable-inputs` list becomes an escape hatch rather than the primary mechanism. Is it worth keeping at all? Recommend yes, for unusual cases (a designer who wants to treat a normally-output signal as adversarial for a particular property), but flag it as escape-hatch-only in docs.

These are flagged as future work, not resolved.

---

## References

**Multi-frontend hardware compiler infrastructure** (the canonical "unify the seams" example).
- CIRCT — Circuit IR Compilers and Tools, [circt.llvm.org](https://circt.llvm.org/). Progressive MLIR lowering from FIRRTL / SV / Calyx dialects to common `hw` / `comb` / `seq` / `sv` dialects.
- HIR: An MLIR-based Intermediate Representation for hardware, [arXiv:2103.00194](https://arxiv.org/pdf/2103.00194).
- K-CIRCT: A Layered, Composable, and Executable Formal Semantics for CIRCT Hardware IRs, [arXiv:2404.18756](https://arxiv.org/html/2404.18756v1).

**yosys hierarchy primitives** that this document's recommendations build on.
- yosys `hierarchy` command — [docs](https://yosyshq.readthedocs.io/projects/yosys/en/stable/cmd/index_passes_hierarchy.html).
- yosys `(* blackbox *)` attribute — same documentation set.
- SymbiYosys (sby) — front-end driver for yosys-based formal flows, [github.com/YosysHQ/sby](https://github.com/YosysHQ/sby).

**Open-source SoC reference** used as the §B.8 example shape.
- OpenTitan — [opentitan.org](https://opentitan.org/) (lowRISC). Formal verification flows under [hw/formal/README.md](https://github.com/lowRISC/opentitan/blob/master/hw/formal/README.md).

**Compositional foundations** (cross-references Document A's References).
- R. Alur, T. A. Henzinger, "Reactive Modules," FMSD 15(1), 1999. [Springer](https://link.springer.com/article/10.1023/A:1008739929481).
- L. de Alfaro, T. A. Henzinger, "Interface Automata," ESEC/FSE 2001. [ACM](https://dl.acm.org/doi/10.1145/503271.503226).
