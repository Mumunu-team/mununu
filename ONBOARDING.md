# Onboarding — mununu

> **Status: Alpha.** mununu is under active development. The fundamentals are stable; the surface keeps moving. Every section of this document is annotated with a **Stability** tag and a **Last reviewed** stamp so you can tell at a glance what to trust. If you read a section and find it stale, file a `doc` finding in `REVIEW_LOG.md` (or fix it — bumping `Last reviewed:` is the only requirement).

This document is what we wish we had on day one. It is **not** a complete reference — the source tree, `CLAUDE.md`, and `docs/` are. It *is* the shortest path from "I have cloned the repo" to "I can ship a feature without breaking something subtle."

## Stability key

| Tag | Meaning | What you can rely on |
|-----|---------|----------------------|
| `stable` | Change requires a deprecation cycle. | The shape, the contract, the name. |
| `evolving` | Shape is known, surface is shifting. **The default in mununu right now.** | The mental model; expect renames and additions. |
| `experimental` | May be removed entirely. | Nothing — read for context, don't build on it. |
| `planned` | Documented for context, not yet implemented. | Only the design doc it points at. |

Sections may also carry `> **Expected change:** <description> — <link>` when a specific upcoming shift is known.

A `Last reviewed:` stamp older than 6 months auto-downgrades the section to `stale — re-verify before relying`.

---

## Table of Contents

1. [Getting Started](#1-getting-started)
2. [The Mental Model](#2-the-mental-model)
3. [Core Concepts](#3-core-concepts)
4. [The Adapter Family](#4-the-adapter-family)
5. [The User Surfaces](#5-the-user-surfaces)
6. [Working in this Codebase](#6-working-in-this-codebase)
7. [Living Architecture](#7-living-architecture)
8. [Glossary](#8-glossary)
9. [Recipes — "I want to add a new X"](#9-recipes)

---

## 1. Getting Started

> **Stability:** —
> **Last reviewed:** — (Chapter 1)

*To be written during Chapter 1 of the review. Should cover: clone, build, the `make ci` contract, the three crates (`mununu-core`, `mununu-cli`, `mununu-extract`) and what each is for, the pre-commit hook setup, your first smoke test against `examples/counters/`.*

---

## 2. The Mental Model

> **Stability:** —
> **Last reviewed:** — (Chapter 1A)

*To be written during Chapter 1A. Should cover, in your words: the three-layer model (extraction → adaptation → verification), the surface-parity rule (CLI / API / UI), the soundness directions (safety + over-approx = sound, etc.), the abstraction-posture vocabulary.*

---

## 3. Core Concepts

> Each subsection has its own stability annotation; the parent has none.

### 3.1 CLTS and KMTS

> **Stability:** —
> **Last reviewed:** — (Chapter 2)

*To be written during Chapter 2. Should cover: states, labels, multi-label edges (one edge ≠ parallel edges), per-state valuations, `LabelControllability`, `Tristate`, `TransitionModality` (Sharp / MayOnly / MustHyperOnly), the `must ⊆ may` invariant. Include one minimal CTXDSL example.*

### 3.2 μ-calculus — Formulas, Guards, Environments

> **Stability:** —
> **Last reviewed:** — (Chapter 3)

*To be written during Chapter 3. Should cover: the `Formula` shape, the five-axis `Guard` (`labels` / `current` / `next` / `control` / `max_steps`), the `Environment` for fixpoint binding, `req_next` / `forb_next` as the under-used primitive worth knowing about.*

### 3.3 μ-calculus — Evaluator

> **Stability:** —
> **Last reviewed:** — (Chapter 4)

*To be written during Chapter 4. Should cover: how to read a 2-valued vs 3-valued verdict, what a witness/lasso trace looks like, the fixpoint inversion rule as a "do not negate the variable" gotcha callout.*

### 3.4 LTL Translation

> **Stability:** —
> **Last reviewed:** — (Chapter 5)

*To be written during Chapter 5. Should cover: which LTL fragment is accepted, where translation lives, the common patterns (`G`, `F`, `U`, response).*

### 3.5 Composition

> **Stability:** —
> **Last reviewed:** — (Chapter 6)

*To be written during Chapter 6. Should cover: each operator (async product / hide / minimize / controllability), the CSP synchronisation rule with a 2x2 diagram, where the `controllability` module fits.*

### 3.6 Abstraction

> **Stability:** —
> **Last reviewed:** — (Chapter 7)

*To be written during Chapter 7. Should cover: how to choose an abstraction posture, where to write `// SOUNDNESS:` comments, the over/under-approximation cheat-sheet for safety vs liveness.*

### 3.7 CTXDSL and `RealizedContext`

> **Stability:** —
> **Last reviewed:** — (Chapter 8)

*To be written during Chapter 8. Should cover: what CTXDSL is, what it is not (it is the IR-as-text, not the source language for new users), the parse → canonicalize → realize pipeline, what a `RealizedContext` carries.*

### 3.8 Synthesis and Diagnostics

> **Stability:** —
> **Last reviewed:** — (Chapter 9)

*To be written during Chapter 9. Should cover: how to read a synthesised controller, the three diagnostic kinds (deadlock / initial failure / vacuous), the difference between counterexamples and counterstrategies.*

### 3.9 Contracts and Codesign

> **Stability:** —
> **Last reviewed:** — (Chapter 13)

*To be written during Chapter 13. Should cover: assume-guarantee in mununu terms, the chaotic-stub default and its direction-dependent soundness, when codesign coupling fires.*

---

## 4. The Adapter Family

### 4.1 Adapter IR contract

> **Stability:** —
> **Last reviewed:** — (Chapter 10)

*To be written during Chapter 10. Should cover: the `AdapterIR` shape, the emit.rs contract (signal-state mode vs explicit-automaton mode), the parse → emit → parse round-trip as the canonical test pattern.*

### 4.2 Per-adapter quick reference

> **Stability:** —
> **Last reviewed:** — (Chapter 11)

*To be written during Chapter 11. One paragraph per adapter: input format, what it abstracts, sidecar requirements (if any), and an individual `Stability:` tag (some adapters are stable, others are evolving or experimental).*

Adapters to cover: SystemVerilog (+ Yosys / BTOR2 / KMTS lift), AIGER, TLSF, Promela, xstate, crewai, langgraph, extraction (.espec.json), library templates (PLIC, watchdog, tracked-memory). The microcode adapter is `planned` — note it but link to the design doc.

### 4.3 The verify orchestrator

> **Stability:** —
> **Last reviewed:** — (Chapter 12)

*To be written during Chapter 12. Should cover: the `verify.toml` schema, alphabet binding rules, what happens when two sources bind the same label.*

---

## 5. The User Surfaces

### 5.1 CLI

> **Stability:** —
> **Last reviewed:** — (Chapter 14)

*To be written during Chapter 14. Should cover: every subcommand at one-line resolution, what each does, where the CLI ends and the library begins. Include the `mununu <cmd> --help`-yourself convention.*

### 5.2 HTTP API

> **Stability:** —
> **Last reviewed:** — (Chapter 15)

*To be written during Chapter 15. Should cover: the endpoint list, request/response shapes, the performance rule (no synthesis in summary endpoints), the timing-log discipline.*

### 5.3 UI

> **Stability:** —
> **Last reviewed:** — (Chapter 16)

*To be written during Chapter 16. Should cover: what the UI consumes from the API, where the typed client lives (`mununu-ui/src/api/endpoints.ts`), what counts as a valid single-surface exception.*

---

## 6. Working in this Codebase

### 6.1 Rust idioms specific to mununu

> **Stability:** stable
> **Last reviewed:** 2026-08-22 (Chapter 0)

The house style is conservative and consistent — read it once, recognize it everywhere. Fourteen patterns worth internalizing (one canonical anchor each; the longer walk with synthetic-vs-misimpl examples lives in the review's Chapter 0 notes):

- **Lifetime-parameterized structs** tie borrow scope to struct validity — [abstraction/evaluator.rs:39](crates/mununu-core/src/abstraction/evaluator.rs#L39).
- **`&str` in params, `String` in owned fields, `&'static str` for compile-time constants** — never `Box::leak` runtime strings in production paths — [library.rs:50](crates/mununu-core/src/library.rs#L50).
- **`Box<dyn Trait> + '_`** trait objects erase concrete iterator types at API boundaries — [iter.rs:10](crates/mununu-core/src/iter.rs#L10).
- **Newtype IDs over `IdStorage`** (`StateId<S>`, `LabelId<L>`) — storage width is a type parameter, ID arithmetic is impossible by construction — [clts/mod.rs:182](crates/mununu-core/src/clts/mod.rs#L182).
- **`#[serde(default)]` + `deny_unknown_fields` + `rename_all = "snake_case"`** on every deserialized API struct — [contract/discover.rs:46](crates/mununu-core/src/contract/discover.rs#L46).
- **`thiserror::Error` + `#[from]`** for error enums; hand-rolled `Display + Error` only when messages need dynamic formatting — [ltl/translator.rs:57](crates/mununu-core/src/ltl/translator.rs#L57).
- **Pattern-first `match` guards** — reserve `if` clauses for runtime predicates, not shape checks a pattern would express — [abstraction/domains.rs:363](crates/mununu-core/src/abstraction/domains.rs#L363).
- **`unsafe { … }` with a `// SAFETY:` line naming the invariant** — every naked block is a review finding — [adapter/yosys/mod.rs:1910](crates/mununu-core/src/adapter/yosys/mod.rs#L2074).
- **`macro_rules!`** used only when a `fn` or a generic won't do (item-emitting macros, ≥ 3 call sites) — [clts/mod.rs:158](crates/mununu-core/src/clts/mod.rs#L158) (`impl_id_storage!`).
- **Consume-self builders** (`mut self` chain → terminal `.finish()`) — no accidental re-use, no partial-state escape — [context/mod.rs:1148](crates/mununu-core/src/context/mod.rs#L1148) (`ContextBuilder`).
- **`RefCell` inside `&self`-only structs** for interned caches — [composition/mod.rs:41](crates/mununu-core/src/composition/mod.rs#L41) (`ProductStateArena`). Prefer `&mut self` when the callsite shape allows.
- **`impl From<X> for Y`** (or `#[from]` on the variant) to enable `?` across module boundaries — [abstraction/unrolling.rs:336](crates/mununu-core/src/abstraction/unrolling.rs#L336).
- **`SmallVec<[T; N]>`** with a comment justifying `N` against measured workload — [clts/mod.rs:264](crates/mununu-core/src/clts/mod.rs#L264) (`[LabelId; 4]` sized against corpus p95).
- **Async axum handlers wrap CPU-heavy work in `tokio::task::spawn_blocking`**, and every handler emits `tracing::info!` per parse / realize / verify phase — [api/handlers.rs](crates/mununu-core/src/api/handlers.rs).

Two crosscutting rules that recur inside those patterns:

- **Never `_ =>` on an internal-crate enum** — the compiler-forced audit when a variant lands is load-bearing; wildcards on external `#[non_exhaustive]` enums are the sole legit case.
- **Never `.unwrap()` on a path that isn't provably unreachable** — prefer `?`, `ok_or_else`, or an `.expect("<invariant>")` that names the reason.

Consume-self and exhaustive-match aren't stylistic choices — they're enforcement mechanisms that push whole classes of bugs from runtime to compile time. When you see them, treat them as load-bearing, not decorative.

### 6.2 The CI gate

> **Stability:** —
> **Last reviewed:** — (Chapter 1)

*To be written during Chapter 1. Should cover: `make ci` is the contract; what it runs; what the pre-commit hook adds; how to reproduce a CI failure locally; what to do when a hook fails (create a new commit, never `--amend`, never `--no-verify` without authorisation).*

### 6.3 Testing pattern

> **Stability:** —
> **Last reviewed:** — (Chapter 10)

*To be written during Chapter 10. Should cover: the three-level adapter testing requirement, the parse→emit→parse round-trip, when to write a unit test vs an integration test, where test_support lives.*

### 6.4 Soundness-comment discipline

> **Stability:** —
> **Last reviewed:** — (Chapter 7)

*To be written during Chapter 7. Should cover: every `eval_expr → None` needs a `// SOUNDNESS:` comment naming over- or under-approximation, why this is enforced by convention rather than the type system (and what would change if it were enforced by type — see Ch 17A verdict on AQ7.1).*

### 6.5 Cross-cutting policies

> **Stability:** —
> **Last reviewed:** — (Chapter 17)

*To be written during Chapter 17. One paragraph each on: Surface Parity, Documentation Traceability, Claims Integrity. Each with the relevant `/parity-check`, `/docs-traceability`, `/soundness-check` skill invocation.*

---

## 7. Living Architecture

> **Stability:** evolving (this whole section is by definition evolving — the alpha admission)
> **Last reviewed:** — (Chapters 1A → 17A)

This section is the alpha-honesty section: it tells you which architectural claims about mununu are currently true, which are currently shifting, and which are open RFCs. **Read it before you assume any specific decomposition is permanent.**

It is populated in two passes: the *initial* shape during Chapter 1A (the architectural claims under test), the *final* shape during Chapter 17A (which claims survived, which were falsified, which became RFCs).

### 7.1 What is stable

*Populated in Ch 17A. List each architectural claim that survived the review unchallenged, with a one-line rationale and the finding ID that confirmed it.*

### 7.2 What is evolving

*Populated in Ch 17A. List each architectural claim that is settled in shape but expected to shift in surface (renames, refactors, additions). For each, the expected change and the issue/RFC link.*

### 7.3 What is experimental

*Populated in Ch 17A. List each architectural area that may be removed or replaced. Anything here is "do not build on it" — name what to use instead.*

### 7.4 What is planned

*Populated in Ch 17A. List capabilities documented in `docs/design/` but not yet implemented (microcode adapter, certain KMTS R.x refinements, etc.). Link the design doc. Do not list them as if they exist.*

### 7.5 Open RFCs from the review

*Populated in Ch 17A. List each `arch`+`bug` finding that was deferred to an RFC, with the RFC link and a one-line description of what it would change.*

---

## 8. Glossary

> **Stability:** —
> **Last reviewed:** —

*Built incrementally — each Core Concepts chapter adds its load-bearing terms. Maintain alphabetical order.*

| Term | One-line definition | Anchor |
|------|---------------------|--------|
| **AdapterIR** | | |
| **CLTS** | | |
| **Controllability class** | | |
| **CTXDSL** | | |
| **Environment** (μ-calculus) | | |
| **Formula** (μ-calculus) | | |
| **Guard** (μ-calculus) | | |
| **KMTS** | | |
| **MayOnly** (transition modality) | | |
| **MustHyperOnly** (transition modality) | | |
| **RealizedContext** | | |
| **Sharp** (transition modality) | | |
| **Sidecar** | | |
| **Soundness posture** | | |
| **Tristate** (Kleene) | | |

---

## 9. Recipes

> **Stability:** —
> **Last reviewed:** —

The "I have done this before, where do I start" section. Each recipe is a numbered checklist; most are 5–8 steps.

### 9.1 Add a new adapter

*Drafted in Chapter 11. Steps: parser → adapter IR → emit round-trip test → CLI hook → API hook → UI client → docs traceability anchor.*

### 9.2 Add a new CLI subcommand

*Drafted in Chapter 14.*

### 9.3 Add a new property template

*Drafted in Chapter 8.*

### 9.4 Ship a feature across all three surfaces

*Drafted in Chapter 15. The surface-parity recipe — CLI handler + API handler + UI client + docs anchor + parity-check skill pass.*

### 9.5 Declare a new soundness posture

*Drafted in Chapter 7. When to use `// SOUNDNESS:`, when to use `AbstractionType::Symbols([…])` in a sidecar, when to add a comment block at the top of a CTXDSL source.*

### 9.6 Add an LTL pattern

*Drafted in Chapter 5.*

---

## Stewardship

This document is alive. To keep it from rotting:

- Any PR that changes a load-bearing primitive, surface, or architectural claim must update the relevant section AND bump its `Last reviewed:` stamp.
- Sections older than 6 months are auto-flagged stale.
- The review process that built this document (see `~/.claude/plans/i-am-looking-the-declarative-snowflake.md`) can be re-run periodically to refresh.
- Use `ShareOnboardingGuide` to publish a shareable link teammates can open without cloning the repo.

When in doubt, prefer truthful "this is currently in flux" over confident-sounding fiction. Alpha software deserves alpha-honest docs.
