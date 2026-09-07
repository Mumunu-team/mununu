# Verifying an SoC that mixes open RTL and closed-IP DDR

> **Draft for Substack publication.** Source: `examples/industrial/dual_frontend_soc/`. The transcript embedded below reproduces byte-for-byte by running `./examples/industrial/dual_frontend_soc/validate.sh` against the pinned commit. **Do not publish until the four-gate validation checklist in `publications/README.md` passes.**

---

Take a small SoC fragment: an open-source host controller that drives an open-source UART peripheral and a closed-IP DDR3 PHY. The host and the UART are yours — written in SystemVerilog, available in source. The DDR PHY arrived as a vendor netlist: a port list, a datasheet, and `(* blackbox *)` on the wrapper. The verification question your customer asks is innocuous: *prove the SoC always reaches its DDR burst path and its UART send path, regardless of what the DDR controller does internally.*

This is the SoC that lives behind every recent automotive infotainment unit, embedded vision system, and edge-AI accelerator. The kernel of formal RTL verification is not "do I trust my own SV?" — it is "what happens when half the design is opaque?"

There are two reasonable ways to answer the question for an SoC like this.

The **protocol-level** answer treats registers as enums, models counters with explicit bounds, and asks "does the handshake sequence ever deadlock?" That answer needs a verifier that preserves symbolic state — losing the abstraction collapses to a state space too large to enumerate.

The **gate-level** answer bit-blasts everything, including the open RTL, and asks "is there a corner case where the carry chain overflows?" That answer needs a verifier that drops every abstraction and reasons over arbitrary widths.

You should not have to pick. The SoC has both kinds of questions; you should be able to ask both. This post walks through how that works in practice — against the example at [`examples/industrial/dual_frontend_soc/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/dual_frontend_soc), reproducible byte-for-byte.

## Two frontends, one IR

mununu, the [open-source compositional model checker](https://github.com/Mumunu-team/mununu) the example runs against, ships two SystemVerilog frontends. One is a Rust-native pipeline that parses a tightly-scoped subset of SV and builds symbolic Kripke structures via SMT — that is the protocol-level path. The other shells out to yosys, runs `flatten`, and bit-blasts the resulting BTOR2 netlist — that is the gate-level path. Same input language, two completely different extraction techniques.

People ask why. The instinct is "one of them must be the wrong call." It isn't. The dual-frontend SoC example is the answer: an SoC with two different verification questions you want answered against two different precision tiers, with one shared CTXDSL definition and one shared composition primitive.

The architectural reference is [CIRCT](https://circt.llvm.org/), the canonical multi-frontend hardware compiler built on MLIR. CIRCT supports multiple frontend dialects (FIRRTL, several SV dialects, Calyx) all lowering through a common middle (`hw`, `comb`, `seq`, `sv`). Different syntaxes, different abstraction tiers, but one IR everything funnels through and one set of passes that operate on the shared dialects. The principle CIRCT made mainstream: **unify the seams, leave the cores free**.

mununu's two pipelines land in the same shape: one IR (`AdapterIR`), one composition primitive, one controllability rule, one set of property roles. The internal extraction techniques — bit-blasting vs SMT-backed Kripke — stay distinct because they serve different precision tiers. The example exercises this principle end-to-end.

## What the SoC example actually does

The SoC has three modules. The `HostController` and `UART` are hand-authored CTXDSL automata representing the open RTL. The `DDR3_PHY_V2` is a 2-state chaotic stub modelling the closed IP — exactly the chaotic-stub default from [Document A](https://github.com/Mumunu-team/mununu/blob/main/docs/design/black-box-modules.md) §2. They compose asynchronously over a shared label alphabet: `req_burst`, `addr_valid`, `rdata`, `ddr_ready`, `ddr_busy`, and the UART's send/receive labels.

The first thing the example walks is how the DDR PHY's interface JSON came into existence. In a real flow, mununu's extractor would emit it automatically when it encountered `(* blackbox *)` in the SV. The example uses `mununu contract sidecars` — the library helper the adapters call — to produce the same JSON shape directly:

```
DDR3_PHY_V2.interface.json     # the port list with directions
DDR3_PHY_V2.gap_report.json    # one OutputSequencing gap
```

The interface JSON is what the discovery pipeline consumes. Running `mununu contract discover DDR3_PHY_V2.interface.json` against it returns:

```
WARN contract gap detected — chaotic stub default in effect
     module="DDR3_PHY_V2" kind=output_sequencing
     labels="rdata, ddr_ready, ddr_busy"
     soundness="safety verdicts hold; liveness verdicts depending on
                these labels are unsound (no progress assumption)."
phase-1 discovery: 8 label(s), 1 gap marker(s) for module `DDR3_PHY_V2`
```

The labels classified as `Uncontrollable` are the ones the host controller cannot drive — exactly the §4 controllability rule applied at the black-box boundary: peripheral outputs are uncontrollable from the host's perspective, peripheral inputs are controllable. The gap report says "you have not authored progress assumptions for the DDR outputs"; the warning makes the soundness consequence explicit.

For a safety-critical product, `mununu contract gaps --strict-contracts` against the gap report exits non-zero. That is the CI gate that prevents shipping a verification result under chaotic-stub semantics without an authored progress contract.

## The verdicts

With the composition wired up, three properties over the SoC:

```
soc_well_formed         →  SAT (1/1 reachable composed states)
burst_path_reachable    →  SAT (host can reach the DDR burst path)
uart_send_reachable     →  SAT (host can reach the UART send path)
```

All three hold *under chaotic DDR*. That is the substantive claim: the host controller is well-formed and can reach its burst and send paths regardless of how the DDR PHY behaves internally — as long as the PHY conforms to its interface alphabet. A liveness property like "every DDR burst eventually completes" would *not* hold under chaotic crypto and would need a vendor-supplied latency-bound contract on `ddr_ready`.

That is the trade-off the chaotic stub forces, and it is the right trade-off. Over-approximation + safety = sound. Over-approximation + liveness = unsound; the system tells you so.

## The principle the example demonstrates

There is a checklist behind this: which parts of the two pipelines were unified at the seams, and which were deliberately left divergent.

The unified items are the ones that should never have been per-pipeline in the first place:

- **Output IR.** Both pipelines produce `AdapterIR`. Already aligned.
- **CTXDSL emission.** Shared via `crates/mununu-core/src/adapter/emit.rs`.
- **Property roles.** The IR has `PropertyRole::{Assumption, Guarantee, Invariant, Standalone}`. The custom-SV path uses them; the yosys path mostly produces `Standalone` today — `chformal -lower` knows the difference between `assert` and `assume`, the BTOR2 reader can keep the distinction.
- **Controllability rule.** The shared classifier lives at `crates/mununu-core/src/controllability.rs`. The custom-SV path uses it; the yosys path defaults to "all inputs uncontrollable" today because `flatten` throws away the port directions even though the SV AST yosys parsed contains them. That gap is the next refactor.
- **Black-box submodule handling.** When either pipeline encounters a `(* blackbox *)` module, it emits `<module>.interface.json` + `<module>.gap_report.json` sidecars next to the CTXDSL output — the same JSON shape mununu's contract subsystem already consumes. **This stage-1 integration is what shipped with M2.** Both frontends call `contract::discover::build_blackbox_sidecars()` today.
- **Composition spec.** Both pipelines emit the same `CompositionSpec` for the same shared design.

The deliberately-divergent items are the ones that exist because the two precision tiers are different verification questions:

- **Abstraction tier.** Symbolic enums vs gate-level bit-exactness.
- **Parser.** Yosys has a mature SV-2017 frontend. Replicating it in Rust would burn engineering years for a tiny gain in homogeneity. Don't.
- **Clock semantics.** Yosys's `async2sync` is a synthesis-grade abstraction; custom SV preserves clock-domain detail.
- **Bit-width.** Bounded counters in custom SV; native arbitrary width in BTOR2.
- **SV language coverage.** Yosys handles classes, interfaces, generates, packages. Custom SV is the opinionated path for SV-written-the-mununu-way.
- **External tool dependency.** Custom SV is pure Rust; yosys is a subprocess. Custom SV is the only path if you can't ship yosys (WASM builds, restricted CI).

Each row in the second list has a real trade-off. Collapsing them would lose half the audience.

## What this example does not claim

Per [mununu's claims-integrity rules](https://github.com/Mumunu-team/mununu/blob/main/CLAUDE.md):

- No claim that any commercial DDR3 PHY or any specific SoC has a bug.
- The example uses `mununu contract sidecars` as a stand-in for adapter auto-emission. The JSON shape is identical to what the yosys-side integration will produce, but the producer is different until that integration lands.
- The example does not exercise vendor `@mununu_guarantee` source-comment annotations. The annotation grammar ships in the [`examples/industrial/tls_handshake/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/tls_handshake) example — the next post in this series. This example deliberately stays at the chaotic-stub baseline so the reader can see what the unannotated default looks like across both RTL frontends.
- The proof is conditional on the chaotic-stub contract; a vendor-supplied latency-bound contract would tighten it.

## Where this fits

The compositional foundations come from Alur & Henzinger's *Reactive Modules* (FMSD 1999) and de Alfaro & Henzinger's *Interface Automata* (ESEC/FSE 2001). The yosys primitives this work builds on (`hierarchy`, `(* blackbox *)`, `cutpoint -blackbox`) are mainstream — used in OpenTitan's formal flows and across SymbiYosys.

What is new is the stage-1 integration between the extractors and the contract subsystem. Until this milestone, the contract subsystem and the extraction pipelines lived in parallel. The contract subsystem could validate discharge graphs and discover phase-1 contracts from `BlackBoxInterface` JSON, but the JSON had to be hand-authored — even though the extractors had just parsed the source and knew exactly what ports each black-box module had. The stage-1 integration is what closes that gap: adapters auto-emit the sidecars, the contract subsystem consumes them, the chaotic-stub default and its diagnostics are now end-to-end.

## What's next

This is post 2 of a four-part series. Each remaining post leads with a different real-world architecture:

- **Post 3 — A TLS handshake driving closed-IP crypto.** When the closed IP is *not unique* (AES-CTR is AES-CTR), a shared corpus replaces per-project contract authoring. Worked example: [`examples/industrial/tls_handshake/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/tls_handshake).
- **Post 4 — A UART driver + UART peripheral.** Cross-boundary HW/SW codesign verification: firmware C + peripheral SV with a register-map sidecar gluing the two sides. Worked example: [`examples/industrial/codesign_uart/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/codesign_uart).

The repo: [github.com/Mumunu-team/mununu](https://github.com/Mumunu-team/mununu).
The example: [`examples/industrial/dual_frontend_soc/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/dual_frontend_soc).
The design doc: [`docs/design/rtl-frontend-unification.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/rtl-frontend-unification.md).

— Mariano Cerrutti
