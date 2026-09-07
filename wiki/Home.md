# Mununu

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

Mununu is a formal verification tool for reactive systems modeled as **Compositional Labeled Transition Systems (CLTS)**. It helps hardware verification engineers and system designers verify protocols, synthesize controllers, and check temporal properties -- all from a single, readable DSL.

Whether you are designing a handshake protocol, an arbiter, a multi-clock-domain pipeline, a sensor network, or an **agentic AI orchestration workflow**, Mununu lets you model the system, compose its parts, state the properties you care about, and get a definitive answer: does the property hold, and if so, can a controller enforce it?

## Key Capabilities

- **CTXDSL** -- a purpose-built DSL for defining automata, alphabets, compositions, formulas, and controllers in one file.
- **Mu-calculus and LTL** -- express safety, liveness, reachability, and fairness properties using mu-calculus directly or via LTL sugar.
- **Synchronous and asynchronous composition** -- combine automata lock-step (shared clock) or interleaved (independent clocks), with hierarchical nesting for cross-domain designs.
- **Controller synthesis** -- automatically derive a maximally permissive controller that enforces a given property, respecting the controllable/uncontrollable label split.
- **Counterstrategy generation** -- when a property is unrealizable, Mununu produces diagnostic traces showing how the environment can force a violation.
- **Format adapters** -- import specifications from XState/Statecharts, SystemVerilog RTL, TLSF, AIGER, Promela, **CrewAI**, and **LangGraph**, plus extraction-spec inputs from C / Rust / TypeScript. Export synthesized controllers back to XState JSON or SystemVerilog modules.
- **N-source verify framework** -- a `verify.toml` manifest composes any combination of the above sources, picks an alphabet-binding strategy, declares a composition shape, and ships a structured `VerifyReport` from CLI / HTTP API / web UI.
- **Web UI** -- the companion `mununu-ui` frontend connects to the built-in API server for interactive graph visualization, formula evaluation, and synthesis. Supports importing adapter formats directly from the file picker.

## Table of Contents

| Page | What you will find |
|------|--------------------|
| [Getting Started](Getting-Started) | Installation, first example, API server |
| [External Tools](External-Tools) | Optional toolchain (sv2v, Yosys, CVC5, Verilator, circt-verilog): the linked-vs-subprocess integration model + licensing posture |
| [CTXDSL Language Reference](CTXDSL-Language-Reference) | Complete DSL syntax: alphabets, automata, transitions, variables, guards, effects |
| [Composition](Composition) | Synchronous, asynchronous, and hierarchical composition with examples |
| [Mu-Calculus Reference](Mu-Calculus-Reference) | Fixpoints, modalities, controllability-aware operators, common patterns |
| [Adapter Formats](Adapter-Formats) | Import from XState, SystemVerilog, TLSF, AIGER, Promela; export controllers |
| [RTL Verification Pipeline](RTL-Verification-Pipeline) | End-to-end SystemVerilog verification with `.mununu.json` sidecars and SMT discovery |
| [Predicate-Cube CEGAR](Predicate-Cube-CEGAR) | Predicate-abstraction CEGAR for large RTL: 3-valued `{T, F, ⊥}` verdicts, automatic refinement, the Caliptra CWE-1245 walkthrough |
| [Exact-Symbolic Engine](Exact-Symbolic-Engine) | Full-state ROBDD model checking: 2-valued definite verdicts (never `⊥`) for `AF` / `AG AF` / `AG EF` / GR(1) liveness, with stall-lasso counterexamples, where predicate abstraction returns `⊥` |
| [Agentic Orchestration](Agentic-Orchestration) | Verify multi-agent workflows, MCP tool authorization, and handoff protocols |
| [Agentic Adapters](Agentic-Adapters) | Native CrewAI + LangGraph JSON parsers — drop a `.crewai.json` / `.langgraph.json` directly into CLI / API / UI |
| [Verify Project Flow](Verify-Project-Flow) | General N-source verification driven by `verify.toml` (CLI / API / UI wizard) |
| [CI and Agent Integration](CI-and-Agent-Integration) | Drive mununu from GitHub Actions (verify verbs as exit-code gates) and from an RTL-writing agent over HTTP |
| [Property Templates](Property-Templates) | Parameterized property patterns (no_deadlock, reachable, bounded, etc.) |
| [CLI Reference](CLI-Reference) | Full command reference with adapter import/export examples |
| [API Reference](API-Reference) | REST API documentation including the import endpoint |
| [Controller Modes](Controller-Modes) | Six controller extraction modes (projection, functional, permissive, signature-memory, product-game, parity-game) and when to use each |

## Video Tutorials

A playlist walking through Mununu from first principles:

[Watch on YouTube](https://www.youtube.com/watch?v=PovNx1rWOy8&list=PL8lIyan4cdjWOUZy32IKu4Yc3Ivi1_YLZ)

## GitHub Repositories

| Repository | Description |
|------------|-------------|
| [mununu](https://github.com/Mumunu-team/mununu) | Backend -- CLI, verification engine, API server |
| [mununu-ui](https://github.com/Mumunu-team/mununu-ui) | Frontend -- web UI for graph visualization, evaluation, and synthesis |

## We'd Love Your Feedback

Mununu is in its early stages and we are actively shaping the tool based on real-world use cases. If you run into a bug, have a feature request, or just want to share how you are using Mununu, please open an issue:

[GitHub Issues](https://github.com/Mumunu-team/mununu/issues)
