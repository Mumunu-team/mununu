# Industrial Value and Validation Domains

> **Status: planning.** This document scopes the classes of industrial verification problems where mununu's KMTS-based 3-valued mu-calculus stack delivers value that incumbent SVA / BMC / commercial-property-checker flows structurally cannot reach. It maps mununu's shipped capabilities (R.1 + R.2 + R.3 + R.4 per the plan's §10.1) onto the load-bearing requirements of seven target domains, and queues seven validation phases (V.0–V.6; V.0–V.5 from the original KMTS plan, V.6 added by the [R.6 replanning pass](../../.claude/plans/r6-controllability-aware-kmts-game-abstraction.md) for controllability-aware synthesis) that anchor the claims to runnable fixtures. Per CLAUDE.md §Documentation Traceability, the capability-inventory section (§2) carries `> Source of truth:` anchors against live code; the domain sections (§3–§8.5) cite the planned fixture paths they ship with the V.x phases. Per the §Phase 5 framework integration of the plan, the GKMTS extension (R.4.5) and the parity-game 3-valued evaluator (R.5.0) precede most of the V.x domain work — V.0 (the "smallest possible coherence" demo) is the only V.x that can ship pre-R.5.

---

## §1 Framing

The pitch is deliberately narrow. mununu is not "a better hardware verifier." mununu is a **domain-targeted verification stack for properties whose structural form — parameterization, hyperproperty character, deep mu/nu alternation, asynchronous composition — lies outside the addressable space of current production tools**, packaged as templates over a semantics expressive enough to capture them and abstractions tractable enough to scale.

mununu's load-bearing capabilities are not theoretical decorations. Each one removes a specific obstacle:

- **Compositional CLTS / KMTS** makes parameterized verification tractable by enabling component-template + environment-abstraction decompositions (the `compose` operator at the [`composition`](../../crates/mununu-core/src/composition/) module).
- **Per-state valuations** (`state_valuation`, `state_3valued_predicates`) lift the model from propositional to data-aware reasoning, so predicate abstraction over real designs is a primitive rather than a hand-rolled escape hatch.
- **Multi-label transitions** (`Transition::labels: SmallVec<[LabelId; 4]>`) allow combined-event modeling (data + acknowledgment + credit + tag) without artificial transition explosion, which is essential for protocol-level reasoning.
- **Sync + async composition** (`CompositionSemantics::{Synchronous, Asynchronous, …}`) matches how real hardware actually composes: handshake-style rendezvous between adjacent components, asynchronous interleaving between independent agents, often within the same design.
- **3-valued KMTS semantics** (`Tristate { KleeneT, KleeneF, KleeneBot }` + `TransitionModality { Sharp, MayOnly }`, with `MustHyperOnly` arriving in R.4.5) is the abstraction substrate that makes the above scale to industrial sizes via the predicate-cube and CEGAR pipelines (R.2 → R.5 → R.5b).

CADP's industrial track record (LNT process algebra + MCL mu-calculus over the same compositional substrate) is the closest existing precedent that these capabilities, properly assembled, do deliver industrial value on the right problem class. mununu extends that line with (a) RTL-first frontends (sv2v + Yosys-no-flatten + BTOR2-per-module per the §3 of [`native-sv-abstraction.md`](native-sv-abstraction.md)), (b) refinement-aware abstraction via predicate cubes + UF wrapping (R.5 + R.5b), and (c) a template-driven user-facing surface that the V.x phases will populate.

---

## §2 Capability inventory — what mununu actually has

> Source of truth: [`crate::composition::compose`](../../crates/mununu-core/src/composition/mod.rs) — surface: API
>
> Source of truth: [`Clts::state_valuation`](../../crates/mununu-core/src/clts/mod.rs#L1369) + [`Clts::state_3valued_predicate`](../../crates/mununu-core/src/clts/mod.rs#L1370) — surface: API
>
> Source of truth: [`Transition::labels`](../../crates/mununu-core/src/clts/mod.rs#L265) — surface: API
>
> Source of truth: [`CompositionSemantics`](../../crates/mununu-core/src/composition/mod.rs) — surface: API
>
> Source of truth: [`Tristate`](../../crates/mununu-core/src/clts/mod.rs#L278) + [`TransitionModality`](../../crates/mununu-core/src/clts/mod.rs#L311) + [`KleeneDom`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) — surface: API

Five mununu primitives sit at the root of every V.x domain claim below:

1. **Compositional `Clts<S, L>` + `compose(left, right, opts)`** — synchronous (label rendezvous), asynchronous (interleaving), and the post-R.4.5 modality-aware merge that respects `MayOnly` / `MustHyperOnly`. Per [`composition/mod.rs:519`](../../crates/mununu-core/src/composition/mod.rs) the rendezvous discipline supports name-equality today and is audited for value-equality semantics under R.1's `composition/mod.rs:519` audit (the second R.1 audit per [`native-sv-abstraction.md`](native-sv-abstraction.md) §6.5).
2. **Per-state structured valuations** — `state_valuation: HashMap<String, String>` for display-only metadata, `state_variable_bitset: Bitset<String>` for the 2-valued AP labelling that drives modal evaluation, and `state_3valued_predicates: Option<BTreeMap<(StateId, String), Tristate>>` (R.1 addition) for KMTS-aware 3-valued AP labels. State valuations survive composition by construction post-R.1.
3. **Multi-label transitions** — every transition holds a `SmallVec<[LabelId; 4]>` of intern'd labels. A single transition can carry `(msg-type, src, dest, tag)` without spurious interleaving. Modal guards (`Guard::labels`) filter on label membership; transition rendezvous in synchronous composition matches on shared labels.
4. **CompositionSemantics: synchronous / asynchronous / mixed** — the same machinery composes a hardware FSM with a software task by interleaving, or composes two RTL modules with shared-net rendezvous. This is the substrate every domain below leans on.
5. **3-valued KMTS semantics** — `Tristate { KleeneT, KleeneF, KleeneBot }` (R.1) for state AP labellings, `TransitionModality { Sharp, MayOnly }` (R.1) plus the R.4.5 addition `MustHyperOnly { targets: SmallVec<[StateId; 4]> }`, evaluated via either the cheap-path fixpoint `evaluate_tri` (R.3) or the planned parity-game `evaluate_3v_game` (R.5.0). `KleeneDom` implements the bulk `EvalDomain` trait per [`evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) (unified P2.2/P2.3).

What's planned (per [`../../.claude/plans/you-are-a-formal-vast-lake.md`](../../.claude/plans/you-are-a-formal-vast-lake.md) §Phase 5):

- **R.4.5** GKMTS extension (`MustHyperOnly` variant + composition rule + modal-operator widening to hyper-targets).
- **R.5.0** 3-valued parity-game evaluator + `FailureSubgame` extraction.
- **R.5** CEGAR via failure-subgame + WP / Craig interpolation predicate splitting + lazy `KmtsLiftLazy` + approximant reuse.
- **R.5b** UF wrapping for wide arithmetic + per-failure-subgame unsat-core partitioning.

The capability-to-domain matrix in §9 below shows how each V.x domain leans on each capability.

---

## §3 Domain 1: Parameterized cache coherence protocols

### Why this is hard for current tools

Coherence protocols (MESI, MOESI, MESIF, directory-based protocols of the FLASH / German / on-die-coherence family) are designed parametrically over an arbitrary number of cache agents. Industrial tools verify finite instances (4, 8, 16 cores) and rely on the engineer's belief that the property scales — a belief that has been wrong often enough to ship coherence bugs in production silicon. Intel's published work on parameterized verification of its many-core on-die coherence protocols notes the protocol "contains complexity that is not present in standard examples" like FLASH or German, and that only a handful of methods can handle even those academic cases.

The hard properties are not the safety invariants (single-writer-multiple-reader, no two M-state copies) — Murphi-style explicit-state methods handle those tolerably. The hard properties are:

- **System-wide deadlock freedom** under arbitrary agent count.
- **Write-eventually-visible liveness** — every write eventually becomes observable to every reader, under realistic fairness assumptions on the interconnect.
- **Combined coherence + interconnect correctness** — the protocol is correct only in conjunction with the underlying message-passing fabric, whose message-dependent deadlock potential is not visible from the protocol in isolation.

### Where mununu's capabilities pay off

| mununu capability | Role in coherence verification |
|---|---|
| Compositional `compose()` | Cache-agent template ⊗ directory template ⊗ network template; parametric instantiation over agent count |
| `CompositionSemantics::Synchronous` | Request / grant / ack handshakes between agent and directory |
| `CompositionSemantics::Asynchronous` | Independent agents interleave local transitions |
| `state_valuation` + `state_3valued_predicates` | Per-agent coherence state, pending request set, outstanding-ack counter |
| Multi-label transitions | `(message-type, data, source, dest, tag)` carried atomically |
| `KleeneDom` + R.4.5 `MustHyperOnly` | Sound abstraction of the "rest of the population" as a may-environment; hyper-must keeps the directory's "send to one of these agents" abstract under refinement |

### Template properties (mu-calculus form)

- **System-wide deadlock freedom**: `νX. ⟨−⟩true ∧ [−]X` over the parametric product. R.5.0's failure subgame, on `KleeneBot`, identifies which agent class's may-but-not-must transitions are blocking convergence.
- **Eventual write visibility under fairness**: `νY. μZ. (visible ∨ ⟨τ⟩Z) ∧ [−]Y` — alternation depth 2; exactly what 3-valued mu-calculus exists for.
- **Mutual exclusion on M-state**: safety invariant lifted through the parameterized composition.

### Validation phase

**V.0** ships the smallest credible coherence demo — a 2-agent MESI fixture with one shared cache line — and validates the safety (`∀X ¬(M(X) ∧ M_other(X))`) and deadlock-freedom properties end-to-end. V.0 is the only V.x that can ship pre-R.5 because Sharp-everywhere semantics suffices for the safety arm and the small fixture's liveness is decidable without CEGAR. **V.4** is the parameterized version that lights up once R.4.5 + R.5.0 + R.5 + R.5b are in place.

Fixtures: [`examples/verify/v0_mesi_2agent/`](../../examples/verify/v0_mesi_2agent/) (V.0, ships with V.0); [`examples/verify/v4_mesi_parametric/`](../../examples/verify/v4_mesi_parametric/) (V.4, ships with V.4).

### Realistic assessment

This is the strongest single domain. Industrial willingness-to-pay is high (coherence bugs cost tape-outs), the property structure genuinely needs the framework's expressiveness, the compositional abstraction matches how engineers already think about the protocols, and the tooling gap is well-documented.

---

## §4 Domain 2: Communication fabrics and NoC liveness

### Why this is hard for current tools

Industrial communication fabrics (AXI, CHI, AMBA-based interconnects, on-chip mesh and torus NoCs, xMAS-style microarchitectural models) routinely contain dozens to hundreds of queues with distributed control. The Verbeek–Schmaltz line of work has documented that deadlock-freedom on such fabrics typically requires either reduction to SAT-style Boolean-equation systems (which scales but is awkward to extend with liveness under fairness) or hand-crafted ACL2 proofs (which scale to topology but not to design churn).

Properties currently handled poorly:

- **Message-dependent deadlock** — a deadlock arising from the *combination* of coherence-protocol and interconnect, not visible from either in isolation.
- **End-to-end liveness under fairness** — every injected packet eventually reaches its destination, assuming weakly-fair routing.
- **Ordering preservation across reconvergent paths** — stream ordering through a network with multiple paths between the same endpoints.
- **Quality-of-service / bounded delivery** — every accepted request delivered within K cycles.

### Where mununu's capabilities pay off

Communication fabrics are *the* natural sync-plus-async target. Channels are sync rendezvous (irdy/trdy in xMAS, valid/ready in AXI); independent flows are async. State valuations capture queue occupancy and credit counts. Multi-label transitions capture combined data-plus-control events without spurious interleaving.

- **Compositional** verification by composing primitive templates (queue, merge, fork, router) is exactly the structure xMAS and similar formalisms already use.
- **GKMTS hyper-must** (R.4.5) lets routing decisions stay abstract: "the router forwards to *some* downstream port consistent with the routing function" without committing to which.

### Template properties

- **Deadlock freedom**: `νX. (∃transition. ⟨−⟩true) ∧ [−]X` — the safety side handled tolerably by current tools, but
- **Liveness under fairness**: `νY. μZ. (delivered ∨ ⟨progress⟩Z) ∧ [−]Y` with fairness encoded as a Streett condition built into the R.5.0 parity game. This is where alternation depth 2 is genuinely required and where SAT-based liveness checking scales but loses the parameterization story.
- **Bounded delivery**: extends the above with a bounded fixed-point on a counter valuation.

### Validation phase

**V.1 (PASS, scoped to a single flow).** Ships a 4-router (2×2) mesh fixture carrying **one** flit from corner R0 to the diagonal corner R3, in two scheduling disciplines (`progress`, `contention`). It verifies **deadlock-freedom** (safety), **end-to-end liveness as possibility** — `AG EF delivered`, the `always_eventually` νμ depth-2 shape, true in both disciplines — and **bounded delivery over a `hops` counter valuation** (`hops <= diameter` invariant + a delivered-at-diameter reachability, both binding through the 2026-06-23 CTXDSL variable-atom path). The discriminating verdict is **strong inevitability** (`AF delivered`): true under `progress`, false under `contention`. mununu imposes no fairness on the abstract stall, so it reports inevitable delivery only for the discipline that actually makes progress — the honest fairness boundary, rather than a Streett-encoded weak-fairness claim the CTXDSL path does not implement. The multi-flit *one-flow-per-router-pair* version (with channel-dependency deadlock and Streett fairness in the R.5.0 parity game) is the parameterized extension the realistic assessment below calls the actual win; it remains future work. Fixture: [`examples/verify/v1_noc_mesh_4router/`](../../examples/verify/v1_noc_mesh_4router/) (ships with V.1).

### Realistic assessment

Second-strongest domain. The compositional structure of fabrics is a perfect match. The realistic win is *parameterized* fabric liveness (verifying a router template for arbitrary mesh size), where current tools require per-instance re-verification.

---

## §5 Domain 3: Memory consistency models and microarchitectural conformance

### Why this is hard for current tools

Industrial memory models (x86-TSO, RVWMO, ARMv8, NVIDIA PTX) are specified axiomatically or operationally and verified against microarchitectural implementations via litmus testing. RealityCheck-style tools enable modular MCM verification but assume specific operational frameworks. The remaining hard problem is verifying that an *implementation* (pipeline + store buffer + cache + coherence) satisfies the consistency model under all programs, not just published litmus tests.

Properties currently underaddressed:

- **All-programs MCM conformance** — not "passes the litmus suite" but "no program can observe a behavior outside the MCM."
- **Composability of MCM proofs** — pipeline TSO + cache TSO doesn't compose without explicit reasoning.
- **Transistency models** — memory ordering interactions with virtual memory, TLB invalidations, dirty bit updates. Recent Princeton / Stanford work (TransForm) has begun formalizing these but tooling is sparse.

### Where mununu's capabilities pay off

- **Multi-label transitions** are essential: a single memory operation carries `(address, data, ordering attributes, fence type, dependency tags)`. Modeling these as separate transitions explodes the state space; modeling them as a single multi-labeled transition keeps the model compositional with the program.
- **Async composition** captures per-thread independence; **sync composition** captures shared-memory and fence synchronization points.
- **State valuations** capture the local store buffer, the per-thread reorder buffer, and the speculative state — none of which are propositional.
- **Compositional** verification lets us verify a memory subsystem template against the MCM abstractly, then instantiate per implementation.

### Template properties

- **SC-per-location**: per-address, writes appear in a total order consistent with program order; encoded as a νμ-formula over composed thread automata.
- **Multi-copy atomicity**: writes from one thread become visible to all other threads at the same logical time — a hyperproperty-like statement reducible to self-composition.
- **Fence semantics**: fences induce the claimed ordering between preceding and following accesses across all threads.

### Validation phase

**V.2** ships a 2-thread store-buffer TSO fixture verifying the canonical Dekker / store-buffering litmus tests, plus SC-per-location for arbitrary programs over a bounded address space. Fixture: [`examples/verify/v2_tso_storebuffer/`](../../examples/verify/v2_tso_storebuffer/) (ships with V.2).

### Realistic assessment

A real wedge, particularly for new RISC-V implementations and custom accelerator MCMs where mature vendor tooling does not exist. Alternation depth is moderate (depth 2 typically suffices); the leverage comes mostly from compositional verification with data-aware abstraction.

---

## §6 Domain 4: Speculative execution security and hyperproperty verification

### Why this is hard for current tools

Spectre-class vulnerabilities are *not* trace properties — they are hyperproperties over pairs of executions that agree on public inputs but may differ on secrets. The standard reduction is self-composition (verify two copies of the design in parallel with constrained inputs), which doubles the state space and stresses current model checkers. SecIC3 (Princeton, 2024–2025) demonstrated that custom IC3 customizations specifically for self-composed designs help, but the broader problem of compositional hyperproperty verification for out-of-order pipelines remains open.

Properties currently underaddressed at the RTL level:

- **Speculative non-interference** — any two executions agreeing on public state produce the same observable trace, even under misspeculation.
- **Contract conformance** — the hardware satisfies a leakage contract specifying which intermediate microarchitectural observations are permitted.
- **Constant-time conformance under speculation** — timing behavior is independent of secret inputs even in the presence of speculative execution.

### Where mununu's capabilities pay off

Hyperproperties on hardware are *almost designed for* sync composition of two design copies with shared input channels. mununu's sync-composition operator gives this naturally. State valuations track taint and speculative state. Multi-label transitions carry the observation labels (cache access, branch outcome, port allocation) that distinguish hyperproperty traces.

A note on framing: it is tempting to say "speculation = may-transitions" — that gloss is too loose. Speculation in real silicon always *happens* (the front-end always fetches and decodes); what is conditionally observable is the microarchitectural side-effect after commit / squash. The right modeling is: speculative-then-squash transitions are `MustHyperOnly { targets }` where targets cover both the commit-effect and the squash-effect successor states; the post-R.4.5 modal-operator semantics treats them per Shoham–Grumberg without requiring an artificial encoding. The 3-valued machinery then captures the conditional observability of microarchitectural state directly.

### Template properties

- **Speculative non-interference**: `□( public_eq → obs_eq )` over the self-composed product, with `obs_eq` being a multi-labeled equality across observation channels.
- **Contract conformance**: `νX. (contract_consistent ∧ [−]X)` — safety on the composed contract-shadow automaton.
- **Bounded leakage under misspeculation rollback**: μ-formula bounding the depth of speculation-induced observations.

### Validation phase

**V.3 (PASS, scoped to the abstract side-channel).** Ships a **self-composed** speculative-load side-channel fixture in two designs (`vulnerable`, `safe`). It demonstrates the modeling pattern the template properties call for: non-interference — a 2-safety hyperproperty `□(public_eq → obs_eq)` — is reduced to ordinary safety on the self-composed product, `never(Leak)`, where `Leak` is the product state in which two copies (equal public input, independent secret) leave **different** cache footprints. The `safe` design squashes the speculative load before it can touch the cache (footprint secret-independent ⇒ `noninterference = true` — the hand-crafted "no secret-dependent cache trace" contract holds); the `vulnerable` design touches `cache[secret]` (`noninterference = false`, with the dual `reachable(Leak) = true` as the non-vacuity witness). Research-grade per the framing note: the pipeline / branch predictor / cache are abstracted to the load-bearing speculation→cache behaviour — it is **not** an RTL pipeline and **not** a production Spectre checker. The RTL-extracted, contract-shadow, and bounded-leakage variants (and a Verilator-confirmed counterexample) remain future work. Fixture: [`examples/verify/v3_specsafe_pipeline/`](../../examples/verify/v3_specsafe_pipeline/) (ships with V.3).

### Realistic assessment

The intellectually most novel application. mununu's 3-valued semantics has a natural interpretation that no other approach captures as cleanly once the modeling is done right (per the framing note above). Realistic adoption is gated by integration with existing RTL flows and by the maturity of hardware-software contract specifications. Strongest as a research wedge with a credible product path 3–5 years out.

---

## §7 Domain 5: GALS systems, clock domain crossings, asynchronous handshakes

### Why this is hard for current tools

Globally-asynchronous-locally-synchronous (GALS) systems and asynchronous handshake circuits (CHP, Balsa, Haste, Click-style controllers) are explicitly modeled with rendezvous semantics. Synchronous-design-oriented tools (JasperGold, VC Formal) handle them awkwardly through clock-domain-crossing constraint sets. CADP-with-LNT-and-MCL handles them natively and is the existing industrial proof point that mu-calculus over a process-algebraic substrate works for this class — but CADP's abstraction story is limited and does not scale to deeply data-dependent asynchronous designs.

Properties currently underaddressed:

- **Token preservation** through async pipelines — every input token eventually produces an output token.
- **Metastability-aware liveness** — liveness conditional on the synchronizer eventually resolving.
- **Mutual exclusion in async arbiters** with non-trivial fairness (bounded-overtaking, weak vs. strong fairness).
- **Conformance of async controller to handshake protocol specification** — 4-phase, 2-phase, bundled-data variants.

### Where mununu's capabilities pay off

This is the domain where async composition is literally the natural model and where CADP-style verification has industrial credibility. mununu's contribution over CADP is the 3-valued data-abstraction layer — state valuations let us abstract data-flow portions of an async pipeline while keeping control-flow exact, which CADP requires the engineer to do manually.

- **Sync composition** for handshake rendezvous (req / ack pairs).
- **Async composition** for independent pipeline stages.
- **State valuations** for token tags, data payloads, credit counts.
- **Multi-label transitions** for combined request / data / tag events on a single channel.
- **3-valued data abstraction** for the data path while keeping control exact.

### Template properties

- **Token preservation**: `νX. (token_count_invariant ∧ [−]X)`.
- **Bounded-overtaking fairness in arbiters**: `νY. μZ. (granted ∨ ⟨τ⟩Z within K) ∧ [−]Y`.
- **Handshake protocol conformance**: μ-formula encoding the 4-phase or 2-phase protocol as a regular expression on actions, lifted via multi-label transitions.

### Validation phase

**V.5** ships a 4-phase async handshake fixture (two communicating CHP-style modules with a request / data / ack channel) and a 3-input mutex fixture with bounded-overtaking, verifying token preservation, conformance, and fairness. Fixture: [`examples/verify/v5_gals_handshake/`](../../examples/verify/v5_gals_handshake/) (ships with V.5).

### Realistic assessment

The clearest near-term industrial application because there is an existing tool (CADP) with a track record, and mununu can position as the next-generation successor with stronger abstraction. Particularly relevant for low-power and security-sensitive designs that increasingly use async logic.

---

## §8 Domain 6: Distributed coordination in hardware accelerators (deferred)

Modern ML accelerators (multi-tile NPUs, dataflow architectures), heterogeneous SoCs, and chiplet-based designs implement distributed coordination protocols in hardware — barrier synchronization, work-stealing schedulers, distributed counters, hardware consensus for coherence directories. These are software-distributed-systems problems implemented at the gate level, and they are verified today mostly through simulation plus a small amount of bounded model checking.

mununu's capabilities map cleanly (parametric tile templates, sync barriers, async intra-tile execution, state-valuation counters, multi-label work-item events), but the validation fixture is **deferred to a second-phase plan** — no V.x is queued for this domain in the present plan. The compositional + parameterized story for Domain 1 (V.4) generalizes to this domain once the V.4 fixture demonstrates the pattern at production scale.

---

## §8.5 Domain 7: Controller synthesis over abstracted reactive systems (V.6)

> Added 2026-06-08 by the R.6 replanning pass ([`../../.claude/plans/r6-controllability-aware-kmts-game-abstraction.md`](../../.claude/plans/r6-controllability-aware-kmts-game-abstraction.md)). Domains 1–6 are *verification* (single-agent KMTS); this domain is *synthesis* (two-player KMTS), which §1–§7 of this doc do not cover.

### Why this is hard for current tools

Reactive controller synthesis with an *abstracted* plant is the gap. GR(1) synthesis tools (Slugs, the original Anzu/RATSY line) are exact on finite Boolean state but have no abstraction-refinement story for data-dependent plants (burst counters, credit windows, address ranges) — the engineer must manually finitize. Abstraction-based model checkers (the KMTS stack of §1–§7) are single-agent: they verify a property of a *closed* system, not synthesize a controller against an adversarial environment. No industrial tool combines *predicate abstraction* with *controller synthesis* soundly, because the soundness story is subtle: the controller's chosen move must be one the abstraction can *vouch for* (a must-edge), while the environment ranges over everything the abstraction *admits* (may-edges). Treating an abstract may-edge as a definite controllable move **over-claims a controller that the concrete system does not admit**.

### Where mununu's capabilities pay off

This is the domain that motivates the controllability-aware KMTS work (R.6): the §7.2 (kmts-theory) 2×2 rule — ∀ over uncontrollable *may*-edges, ∃ over controllable *must*-edges — is exactly the sound mixing GR(1)-over-abstraction needs.

- **Per-label controllability** (`LabelControllability`) for the environment/controller input partition.
- **3-valued (may/must) transition modality** for the abstracted datapath.
- **μ-calculus alternation** for GR(1) response liveness (νμ).
- **CEGAR refinement** keyed on the *owning player* of the uncertain edge (kmts-theory §7.6): an uncertain controllable edge drives must-growth (confirm the move); an uncertain environment edge drives may-shrinking (rule out a phantom adversary move).

### Template properties

- **GR(1) response (request → eventual grant)**: `νY. μZ. ((¬request ∨ granted) ∧ [(ctrl = environment)] ⟨(ctrl = controllable)⟩ (Y ∧ Z-progress))` — the box ranges over environment may-edges, the diamond over controllable must-edges.
- **Safety under adversarial environment**: `νX. (safe ∧ [(ctrl = environment)] ⟨(ctrl = controllable)⟩ X)` — controller maintains an invariant against all admitted environment moves.

### Validation phase

**V.6** ships the controllability-aware proof-of-fire (R.6.7): a **hand-authored AMBA-style arbiter** (synthesisable Verilog + equivalent BTOR2) where the burst-length counter is predicate-abstracted to produce `MayOnly` edges, the request/grant signals split into uncontrollable/controllable inputs, and the response-liveness property is GR(1). The done-criterion is a demonstrated **divergence**: the modality-blind verdict path returns a spurious "controller exists," while the controllability-aware path (R.6.3) returns a sound `KleeneBot`-then-refine verdict. Fixture: [`examples/verify/v6_controllability_kmts/`](../../examples/verify/v6_controllability_kmts/) (ships with V.6 at the 2026-06-09 partial; see §"V.6 fixture-provenance" below for the path-chosen rationale).

### V.6 fixture-provenance (added 2026-06-09 per the R.6.7 path-chosen analysis)

The original V.6 framing assumed the public **AMBA AHB from the GR(1)/TLSF corpus** (Bloem–Jobstmann–Piterman–Pnueli–Sa'ar). The 2026-06-09 R.6.7 fixture-path analysis found this requires infrastructure mununu does NOT have: the TLSF adapter (`crates/mununu-core/src/adapter/tlsf/mod.rs`) goes directly TLSF → CTXDSL (Sharp-only); the path to KMTS with predicate-abstraction-induced MayOnly edges requires BTOR2 input, and TLSF → BTOR2 (with a predicate-abstractable datapath) requires either (a) an external GR(1) synthesiser producing the controller mununu is supposed to *verify*, or (b) hand-authored Verilog.

The V.6 fixture-path chosen is **Option B (hand-authored Verilog with predicate-abstractable burst counter)**, per CLAIM Integrity labeled as a hand-authored fixture demonstrating the verdict-divergence pattern on real RTL semantics rather than a SYNTCOMP-corpus claim. The R.6.6 controllability-aware lifter + R.6.3 modality-aware evaluator are exercised end-to-end on the actual production code paths (5 integration tests confirm the lift produces BOTH controllable labels AND MayOnly edges from the same source); the only difference vs the original framing is the fixture provenance.

The path that *would* connect to the public TLSF corpus — TLSF → controller-RTL via external synthesis (Strix / BoSy / similar) → BTOR2 → predicate-cube lift — is a multi-week infrastructure expansion currently parked as a follow-up. It would couple mununu to external synthesisers in a way the rest of the verification stack does not, so the engineering trade-off is genuinely structural.

### Realistic assessment

The newest and least-proven domain. V.6 partial-shipped 2026-06-09: R.6.3–R.6.6 evaluator stack closed, V.6 Option B fixture demonstrates end-to-end controllability-aware lift + R.2.5 MayOnly emission on RTL semantics (5 integration tests pass; `validate.sh` runs the CLI invocation end-to-end with the new `--controllable` flag). Remaining: mu-calc safety/liveness property authoring + verdict-divergence demonstration + mununu-ui SV-source workflow + tutorial. The honest-state claim today is *partial demonstration of the controllability-aware verdict-divergence pattern on a hand-authored RTL fixture*, not yet a full SYNTCOMP-corpus AMBA result. Its strategic value remains unique: it is the only V.x that exercises mununu as a *synthesis* tool over abstraction, which no incumbent does.

---

## §9 Capability-to-domain matrix

| Domain (V.x) | Compositional | State valuations | Multi-label | Sync+Async | 3-valued / GKMTS | μ-cal alternation |
|---|---|---|---|---|---|---|
| V.0 / V.4 Coherence | Critical | Critical | High | Critical | Critical | Yes (νμ) |
| V.1 NoC | Critical | High | High | Critical | High | Yes (νμ) |
| V.2 Memory consistency | High | Critical | Critical | High | Moderate | Yes (νμ) |
| V.3 Speculative security | High | Critical | High | Critical (self-comp) | Critical | Moderate (mostly ν) |
| V.5 GALS / async | High | High | Critical | Critical | High | Yes (νμ) |
| V.6 Synthesis over abstraction † | Moderate | High | High | High | Critical | Yes (νμ, GR(1)) |
| (deferred) Distributed accelerator | Critical | High | High | Critical | High | Yes (νμ) |

† V.6 additionally requires **per-label controllability** (the environment/controller partition) composed with the 3-valued/GKMTS column — the controllability × may/must product of [`kmts-theory.md`](kmts-theory.md) §7. It is the only V.x that exercises mununu as a *synthesis* tool; the matrix columns above are verification-oriented and do not capture the controllability axis on their own.

Every shipped + planned capability earns its keep in at least three V.x domains; no capability is decorative.

---

## §10 Honest scope and limitations

mununu is **not** a replacement for SVA-based industrial flows on the 80–85% of properties they handle well. The V.x domains above represent the 5–10% where current tools either fail to terminate, lose parameterization, cannot express the property structure, or require months of manual proof engineering. The value is concentrated, not distributed.

Hard constraints mununu does not relax:

- **Tool qualification** for ISO 26262 / DO-254 / IEC 61508 remains a separate, expensive, multi-year investment. A domain-targeted product is easier to qualify than a general checker, but it is not free. mununu has no qualification posture today.
- **Non-termination of CEGAR refinement** on infinite-state abstractions remains possible; the R.5 bounded-iteration policy (16 rounds default per §10.1) + graceful fallback to `KleeneBot` with a soundness-tagged warning is the production behavior.
- **Template library coverage** is the dominant engineering investment — the V.x phases ship one fixture per domain (small, representative). Production depth in any one domain is 5–10 engineer-years beyond the V.x demonstration.
- **Counterexample / indefinite-verdict presentation** in domain-natural terms (waveform traces, protocol-level diagnostics) is per-domain UI work that lands in U.0 + post-U.0 follow-ups, not a free byproduct of the formalism.

---

## §11 Strategic prioritization

Concentrate initial validation investment on two domains where the wedge is sharpest and the existing-tooling gap is most documented:

1. **V.0 → V.4: parameterized cache coherence** — highest willingness-to-pay, clearest property structure match, strongest published evidence of the gap. V.0 is achievable pre-R.5; V.4 closes the parametric story post-R.5b.
2. **V.5: asynchronous / GALS verification with data abstraction** — clearest predecessor (CADP), shortest path to a working prototype, smallest gap between research and industrial usability.

V.1 (NoC), V.2 (MCM), V.3 (speculative security) are deferred to second-phase investment once V.0 / V.4 / V.5 demonstrate the substrate. Domain 6 (distributed accelerator coordination) is deferred to a second-phase plan. **V.6 (synthesis over abstraction)** is sequenced last and gated on the full R.6 arc landing (the sound controllability-aware evaluator does not yet exist in the production verdict path) — it is a high-risk, high-uniqueness bet rather than a near-term wedge.

External framing for mununu post-V.5: *"a verification stack for parameterized and asynchronous hardware coordination, complementing existing SVA-based flows on problems they structurally cannot reach."* This framing is defensible against any incumbent comparison because it does not compete with incumbents on their strong ground.

---

## §12 Closing

mununu's load-bearing capabilities — compositional, valuation-aware, multi-labeled, sync-and-async — are not chosen for theoretical elegance. Each one is the load-bearing element of at least three concrete industrial verification problems where current tools are inadequate. The 3-valued KMTS semantics is the connective tissue that makes abstraction-refinement work on these models. The mu-calculus is the property language because the properties of interest genuinely have the alternation structure mu-calculus exists to capture.

The honest claim is not "mununu verifies critical industrial hardware." The honest claim is: **for a specific, identifiable, valuable class of hardware verification problems characterized by parameterization, asynchronous composition, hyperproperty structure, or deep liveness alternation, mununu occupies a wedge that no industrial tool today addresses, and the wedge is wide enough to support a credible product line.** That claim is defensible. Anything broader is not.

The V.0–V.5 validation phases (next; see plan §Phase 7), plus V.6 (synthesis over abstraction; §8.5, gated on the R.6 arc), anchor this claim to runnable fixtures, with the same blocker-protocol discipline (§10.2 of the plan) that gates the R.x / M.x track. **A V.x failure means the domain claim is downgraded or retracted, not that the fixture is hand-translated** — same evidence discipline.
