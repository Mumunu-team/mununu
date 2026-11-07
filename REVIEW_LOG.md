# mununu Hand-Review Log

Companion to `~/.claude/plans/i-am-looking-the-declarative-snowflake.md`.
Append-only. One H2 section per chapter. Roll up `bug`-severity findings at the end when all chapters are `done`.

## Conventions

### Severity vocabulary (fixed)

- **`bug`** — incorrect behaviour, soundness violation, or contract breach. For `arch`-category findings, `bug` means *the codebase violates a stated architectural contract* (e.g. an adapter reaches into another adapter; a summary endpoint runs synthesis). Must be filed as a follow-up before the chapter can be marked `done` (architecture `bug`s block Ch 17A, not the chapter where they were found).
- **`concern`** — design / clarity / documentation issue that would block a new contributor; not blocking for the review itself.
- **`note`** — observation, "huh interesting", possible future cleanup.

### Category vocabulary (fixed)

- **`behavior`** — code does the wrong thing (or a non-obvious right thing worth documenting). Fix near where found.
- **`arch`** — decomposition / layering / dependency-direction / primitive-choice issue. Do *not* fix mid-review; accumulate evidence and decide in Chapter 17A.
- **`doc`** — drift between documentation and code (anchor for `/docs-traceability`). Often a quick fix; sometimes signals an `arch` issue underneath.

### Finding ID format

`CC-FNN` where `CC` is the chapter number (zero-padded, `00`–`17`, or `1A` / `17A` / `11a`–`11f` for letter-suffixed chapters) and `NN` is sequential within the chapter. Examples: `02-F01`, `11a-F03`, `17A-F02`.

### Status vocabulary

- Chapter status: `not started` → `in-progress` → `blocked` | `done`
- Finding status: `open` → `triaged` → `fixed in <sha>` | `wontfix (<reason>)` | `RFC <link>` (for `arch` findings deferred to 17A)

### Per-chapter template

```markdown
## Chapter NN — <title>

Started: YYYY-MM-DD
Reviewer: <your name>
Status: in-progress

### Acceptance Criteria
- [ ] Comprehension Q1: …
- [ ] Smoke test: <command>
- [ ] (sensitive only) Invariant I1: …
- [ ] (sensitive only) Architecture Question AQ1: <answer in log below>
- [ ] **Onboarding contribution**: `ONBOARDING.md` §<from plan's contribution table> updated, `Stability:` annotation set, `Last reviewed:` stamped

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A (sensitive chapters only)
- AQ1: <question> — <yes/no/tradeoff> — <evidence cite> — <action if not yes>

### Decisions
-

### Follow-ups
-
```

---

## Chapter 0 — Rust Orientation Primer

Started: 2026-08-20
Completed: 2026-08-22
Reviewer: Mariano Cerrutti
Status: done

### Acceptance Criteria
- [x] Explain patterns 9–22 in your own words
- [x] Smoke test: `cargo build --workspace && cargo test --workspace -- --skip slow`
- [x] First finding logged (even a `note`) so the log mechanism is working
- [x] **Onboarding contribution**: `ONBOARDING.md` §6.1 (Rust idioms) updated with a ~200-word recap of patterns 9–22, `Stability:` set (`stable`), `Last reviewed:` stamped (2026-08-22)

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|
| 00-F01 | note | arch | crates/mununu-core/src/adapter/btor2/ast.rs:388 | Calling a method with `self` by value (not `&self` / `&mut self`) is a first-class API design tool. It enforces at compile time: (a) transformations that must invalidate the original binding (the caller cannot keep using the pre-transform value); (b) builder patterns that consume the builder at `.build()` / `.finish()` (no double-build, no partial-state builder escaping); (c) "use once" semantics on tokens/handles/permits that cannot be legitimately reused. When you see `pub fn foo(self, …) -> …`, treat it as a load-bearing signal, not a stylistic choice. | open |
| 00-F02 | note | arch | crates/mununu-core/src/ltl/ast.rs:8 | Exhaustive `match` over an `enum` with **no `_ =>` wildcard** turns "handled every case" from a runtime assumption into a compile-time proof. When a new variant lands, the compiler surfaces every unhandled site; a wildcard hides them and forces the failure mode to become either a runtime `panic!` or a silent `Ok(())` / `default()` drop that swallows the case. Wildcards are legitimate only when the enum is `#[non_exhaustive]` from an external crate; internal-crate enums should default to no-wildcard so refactors are compiler-audited. | open |

### Decisions
-

### Follow-ups
-

---

## Chapter 1 — Workspace, Build, CI Gate

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] `make ci` green on `main`
- [ ] List three crates, their bin/lib status, and feature flags from memory
- [ ] Pre-commit hook installed and understood
- [ ] Smoke test: `make ci`
- [ ] Smoke test: `cargo build --workspace --release`
- [ ] Smoke test: `mununu --help`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 1A — Architecture Baseline (opening bookend)

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] §1 Dependency DAG drawn in this entry (ASCII), back-edges flagged
- [ ] §2 Primitive inventory written (one sentence each for `Clts`, `LabelId`, `StateId`, `LabelControllability`, `TransitionModality`, `Tristate`, `Formula`, `Guard`, `Environment`, `AdapterIR`, `RealizedContext`, `Context`, `Contract`)
- [ ] §3 User surfaces with their contracts
- [ ] §4 At least 6 architectural claims with a paired falsifiability hook ("how would I know this is false?")
- [ ] §5 At least 4 open architecture questions, each with a back-pointer to the chapter that will most likely answer it
- [ ] First `arch`-category finding logged (a `note` is fine) so the schema is exercised

### §1 Dependency DAG (your reconstruction)

```
(write the DAG here after reading docs/design/ and crates/mununu-core/src/lib.rs)
```

Back-edges noted:
-

### §2 Primitive inventory

| Type | Responsibility (one sentence) |
|------|-------------------------------|
| `Clts` | |
| `LabelId` | |
| `StateId` | |
| `LabelControllability` | |
| `TransitionModality` | |
| `Tristate` | |
| `Formula` | |
| `Guard` | |
| `Environment` | |
| `AdapterIR` | |
| `RealizedContext` | |
| `Context` | |
| `Contract` | |

Suspected overlaps (record now; will be checked across the review):
-

### §3 User surfaces and their contracts

- **CLI**:
- **HTTP API**:
- **UI**:

### §4 Architectural claims to be tested

| # | Claim | Falsifiability hook (how would I know this is false?) |
|---|-------|--------------------------------------------------------|
| C1 | | |
| C2 | | |
| C3 | | |
| C4 | | |
| C5 | | |
| C6 | | |

### §5 Open architecture questions

| # | Question | Likely-answered-in |
|---|----------|---------------------|
| AQ-1.1 | | |
| AQ-1.2 | | |
| AQ-1.3 | | |
| AQ-1.4 | | |

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 2 — CLTS Data Model `[SENSITIVE]`

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: why is `MustHyperOnly` boxed and other variants not?
- [ ] Comprehension Q: what does it mean for a CLTS to have *no* `state_valuation` for a given state?
- [ ] Smoke test: `cargo test -p mununu-core clts::`
- [ ] Smoke test: `cargo test -p mununu-core --test r3_kleene_baseline`
- [ ] **I2.1**: no adapter / lifter constructs `TransitionModality::MustHyperOnly(_)` (only `composition::*` does) — `rg "MustHyperOnly\(" crates/mununu-core/src/adapter/` returns nothing surprising
- [ ] **I2.2**: `must ⊆ may` invariant — no constructor produces "must without may"
- [ ] **I2.3**: `LabelControllability` never encoded as label-name suffix (`_ctrl_`, `_unctrl_`, `_env_` grep returns only test fixtures)
- [ ] **I2.4**: multi-label edges built as one transition (spot-check `adapter::aiger`)
- [ ] **I2.5**: `Tristate::KleeneBot` only demoted to definite verdict via documented paths
- [ ] **AQ2.1** answered in §Architecture Q&A
- [ ] **AQ2.2** answered in §Architecture Q&A
- [ ] **AQ2.3** answered in §Architecture Q&A
- [ ] **AQ2.4** answered in §Architecture Q&A

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A
- **AQ2.1 — Module boundary**: Should `Tristate` and `TransitionModality` live inside `clts/mod.rs`, or be promoted to a sibling `kmts/` module?
  - Answer:
  - Evidence:
  - Action:
- **AQ2.2 — Primitive overlap**: Do `state_variable_bitset`, `state_valuation`, and `state_3valued_predicates` represent three views of the same information, or three distinct facts?
  - Answer:
  - Evidence:
  - Action:
- **AQ2.3 — Type-level vs comment-level invariants**: Could `must ⊆ may` be enforced by construction (private fields, smart constructors) rather than by absence-of-constructor?
  - Answer:
  - Evidence:
  - Action:
- **AQ2.4 — `IdStorage` genericity**: Is parameterising every CLTS over `S: IdStorage` paying for itself, or is everyone using `DefaultStateIdx`?
  - Answer:
  - Evidence:
  - Action:

### Decisions
-

### Follow-ups
-

---

## Chapter 3 — μ-calculus AST, Environment, Guards

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: draw the set of transitions constrained by `[(labels = {a}, req_next = {active}, ctrl = controllable)] φ`
- [ ] Comprehension Q: difference between `req_cur` and `req_next` semantically
- [ ] Smoke test: `cargo test -p mununu-core mu_calculus::`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 4 — μ-calculus Evaluator `[SENSITIVE]`

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: hand-walk `νX. p ∧ [a]X` on a 3-state automaton, then confirm against `cargo run`
- [ ] Smoke test: `cargo test -p mununu-core --test mu_calculus_response_pattern`
- [ ] Smoke test: `cargo test -p mununu-core --test r3_kleene_baseline`
- [ ] Smoke test: `mununu context eval examples/counters/counters.ctxdsl --sidecar examples/counters/counters_properties.ctxdsl --formula reachability --automaton Counter`
- [ ] **I4.1**: fixpoint inversion rule — `¬(μX. φ) → νX. ¬φ`, body's `X` references NOT negated; confirm with witness test
- [ ] **I4.2**: witness/strategy extraction returns lasso traces (prefix + cycle); cycle non-empty for liveness witnesses, prefix may be empty
- [ ] **I4.3**: `evaluate_tri` never silently collapses `KleeneBot`; every demotion commented
- [ ] **I4.4**: `evaluate` over sharp-everywhere KMTS = identical results to 2-valued evaluator (semantic regression)
- [ ] **AQ4.1** answered in §Architecture Q&A
- [ ] **AQ4.2** answered in §Architecture Q&A
- [ ] **AQ4.3** answered in §Architecture Q&A
- [ ] **AQ4.4** answered in §Architecture Q&A

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A
- **AQ4.1 — Truth-domain parametricity**: Could `evaluate` and `evaluate_tri` be one function parameterised over `TruthDomain`?
  - Answer:
  - Evidence:
  - Action:
- **AQ4.2 — Witness extraction coupling**: Should witness extraction and counterstrategy emission share a common abstract output, or is lasso-vs-automaton fundamental to liveness-vs-safety witnessing?
  - Answer:
  - Evidence:
  - Action:
- **AQ4.3 — Fixpoint inversion as a separate pass**: Should the inversion transformation be a named single-source-of-truth function?
  - Answer:
  - Evidence:
  - Action:
- **AQ4.4 — Modality five-axis guard as one type**: Are all five `Guard` axes always used together, or is the struct "wide" (common case is 1–2 axes)?
  - Answer:
  - Evidence:
  - Action:

### Decisions
-

### Follow-ups
-

---

## Chapter 5 — LTL → μ-calculus Translation

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: hand-translate `G(req → F grant)` and compare with translator output
- [ ] Comprehension Q: what LTL fragment is *not* supported? (find `TranslationError` arms)
- [ ] Smoke test: `cargo test -p mununu-core --test ltl_patterns`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 6 — Composition `[SENSITIVE]`

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Smoke test: `cargo test -p mununu-core composition::`
- [ ] Smoke test: `mununu context eval examples/counters/counters.ctxdsl --formula reachability --automaton Counter`
- [ ] **I6.1**: shared alphabet synchronises, independent labels interleave (CSP rule); confirm with 2x2 hand example
- [ ] **I6.2**: `Internal` controllability mutually-exclusive between automata in composition; confirm composition test where one side has `Internal a` and the other `Controllable a`
- [ ] **I6.3**: `TransitionModality::MustHyperOnly` placeholder from `merge` IS realized in composition layer; grep for `merge_with_hyper_targets`
- [ ] **I6.4**: bisimulation minimization preserves μ-calculus semantics on surviving label set (regression test)
- [ ] **AQ6.1** answered in §Architecture Q&A
- [ ] **AQ6.2** answered in §Architecture Q&A
- [ ] **AQ6.3** answered in §Architecture Q&A
- [ ] **AQ6.4** answered in §Architecture Q&A

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A
- **AQ6.1 — `composition::controllability` vs top-level `controllability`**: Clean producer/consumer split or duplicated logic?
  - Answer:
  - Evidence:
  - Action:
- **AQ6.2 — Operator catalogue completeness**: Is the composition operator set what CTXDSL exposes? Any user-askable operator that doesn't exist (or vice versa)?
  - Answer:
  - Evidence:
  - Action:
- **AQ6.3 — `ProductStateArena` interior mutability**: Could `RefCell<HashMap<...>>` be replaced with an explicit `&mut` builder?
  - Answer:
  - Evidence:
  - Action:
- **AQ6.4 — Layer position of minimization**: Should bisimulation minimization live next to `clts/` rather than under `composition/`?
  - Answer:
  - Evidence:
  - Action:

### Decisions
-

### Follow-ups
-

---

## Chapter 7 — Abstraction `[SENSITIVE]`

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Smoke test: `cargo test -p mununu-core abstraction::`
- [ ] **I7.1**: every `eval_expr → None` site in abstraction layer has nearby `// SOUNDNESS:` comment naming over/under approximation
- [ ] **I7.2**: abstraction posture declared per adapter (spot-check three from Ch 11)
- [ ] **AQ7.1** answered in §Architecture Q&A
- [ ] **AQ7.2** answered in §Architecture Q&A
- [ ] **AQ7.3** answered in §Architecture Q&A
- [ ] **AQ7.4** answered in §Architecture Q&A

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A
- **AQ7.1 — Comment-discipline vs type-level soundness**: Could `eval_expr` return `EvalResult { value, posture }` so the posture is non-optional by construction?
  - Answer:
  - Evidence:
  - Action:
- **AQ7.2 — Layer position of abstraction**: Producer, consumer, or both? If both, is the bidirectional dependency a smell?
  - Answer:
  - Evidence:
  - Action:
- **AQ7.3 — Per-subsystem recipe duplication**: Are the per-subsystem recipes encoded as code (library templates) or as prose only?
  - Answer:
  - Evidence:
  - Action:
- **AQ7.4 — `ExpressionEvaluator<'a>` lifetime cost**: Any callsite that would benefit from owned state?
  - Answer:
  - Evidence:
  - Action:

### Decisions
-

### Follow-ups
-

---

## Chapter 8 — CTXDSL: Parser → Realize → RealizedContext

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: trace one CTXDSL source from text to `RealizedContext` — which functions touch it?
- [ ] Comprehension Q: what does the canonicalizer guarantee post-pass?
- [ ] Smoke test: `cargo test -p mununu-core context_dsl::`
- [ ] Smoke test: `cargo test -p mununu-core --test context_dsl_controllability`
- [ ] Smoke test: `mununu context parse examples/counters/counters.ctxdsl`
- [ ] Smoke test: `mununu context canonicalize examples/counters/counters.ctxdsl`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 9 — Controller Synthesis & Diagnostics `[SENSITIVE]`

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Read `docs/synthesis.md` end-to-end, especially Skolem-paradigm rules
- [ ] Smoke test: `cargo test -p mununu-core --test diagnostics`
- [ ] Smoke test: `cargo test -p mununu-core --test scalable_gr1`
- [ ] Smoke test: `mununu context synth <GR1 example>` produces non-empty controller
- [ ] **I9.1**: strategy extraction uses signature-based selection from iteration ranks
- [ ] **I9.2**: synthesis-failure diagnostic distinguishes deadlock trace / initial-state failure / vacuous spec
- [ ] **I9.3**: counterstrategy emission produces CLTS-like structure, not single trace
- [ ] **I9.4**: counterexamples (lasso traces) distinct from counterstrategies (witness automata) in API response shape
- [ ] **AQ9.1** answered in §Architecture Q&A
- [ ] **AQ9.2** answered in §Architecture Q&A
- [ ] **AQ9.3** answered in §Architecture Q&A
- [ ] **AQ9.4** answered in §Architecture Q&A
- [ ] **AQ9.5** answered in §Architecture Q&A

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A
- **AQ9.1 — Synthesis layer placement**: Should synthesis move out of `context/mod.rs` into a sibling `synthesis/` module?
  - Answer:
  - Evidence:
  - Action:
- **AQ9.2 — Parity-game solver as its own crate**: Should the Zielonka / signature-extraction code be extracted to a `mununu-parity` crate?
  - Answer:
  - Evidence:
  - Action:
- **AQ9.3 — Diagnostic shape uniformity**: Sum type with one canonical encoding, or three parallel structs?
  - Answer:
  - Evidence:
  - Action:
- **AQ9.4 — Witness vs counterexample vs counterstrategy**: Same underlying notion under three names, or genuinely three artefacts?
  - Answer:
  - Evidence:
  - Action:
- **AQ9.5 — Assumption-discovery output shape**: Does `--discover-assumptions` (per memory `refined-verdicts-assumption-discovery`) emit a single strongest assumption or a ranked set of candidates? For each discovered assumption, what metadata is attached — the two-sided non-vacuity gate result, a weakness/strength ordering, the discovering engine, the motivating counterexample? If single-best, would a ranked-set output improve downstream utility (human triage, repair loops, differential-oracle cross-check)? A single-best output also makes it hard to record *rejected* candidates and *why*.
  - Answer:
  - Evidence:
  - Action:

### Decisions
-

### Follow-ups
-

---

## Chapter 10 — Adapter IR + CTXDSL Emitter

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: which emitter mode produces multi-label edges, which produces parallel single-label edges, and why?
- [ ] Smoke test: `cargo test -p mununu-core --test adapter_cross_format`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 11 — Adapter Walkthrough

Started:
Reviewer:
Status: not started

> One subsection per adapter. Findings namespaced as `11a-FNN`, `11b-FNN`, etc.

### Per-adapter acceptance criteria (applies to every subsection)
- [ ] Parse a real example without error
- [ ] Emitted CTXDSL canonicalises losslessly (parse → emit → parse → equal)
- [ ] Multi-label edges are *one* transition where source format has multi-action (Ch 2 I2.4)
- [ ] Abstraction posture declared somewhere reachable from the adapter (Ch 7 I7.2)

### 11a — SystemVerilog → Kripke (+ Yosys → BTOR2 → KMTS)
- [ ] Smoke test: `cargo test -p mununu-core --test btor2_kmts_lift_sweep`
- [ ] Smoke test: `cargo test -p mununu-core --test sv_preprocess_sweep`
- [ ] Smoke test: `mununu sv discover examples/systemverilog/alu.sv`
- [ ] **I11a.1** (sensitive): BTOR2 KMTS lifter never emits `MustHyperOnly`
- [ ] **I11a.2**: every cross-module register read annotated as `KleeneBot` until sidecar resolves

### 11b — AIGER / TLSF
- [ ] Smoke test: `mununu context eval examples/aiger/alarm.aag --formula safety_alarm_on --automaton Circuit`

### 11c — Promela
- [ ] Smoke test: `mununu context eval examples/promela/mutex_simple.pml --formula mutex --automaton System`

### 11d — Agentic (xstate / crewai / langgraph)
- [ ] Smoke test: walk one example from `examples/agentic/` end-to-end

### 11e — Extraction (.espec.json → CTXDSL)
- [ ] Smoke test: `mununu context import examples/ast_extract/<one>/*.espec.json`
- [ ] Walk both directions: `mununu-extract` (producer) and `adapter::extraction` (consumer)

### 11f — Library templates (PLIC, watchdog, tracked-memory)
- [ ] Smoke test: `mununu library list`
- [ ] Smoke test: `mununu library emit plic …`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 12 — Verify Orchestrator

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Comprehension Q: trace one `verify.toml` end-to-end — which file calls which adapter, then which composition, then which evaluator?
- [ ] Comprehension Q: what happens if two sources bind the same label?
- [ ] Smoke test: `find examples -name verify.toml | head -3`
- [ ] Smoke test: `mununu verify <that path>`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 13 — Contracts, Codesign, Black-Box Modules `[SENSITIVE]`

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Smoke test: `cargo test -p mununu-core contract::`
- [ ] Smoke test: `mununu contract validate <example>`
- [ ] Smoke test: `mununu codesign <example>`
- [ ] **I13.1**: chaotic-stub default is over-approximation for environment-side missing contracts, under-approximation for system-side; verify both directions
- [ ] **I13.2**: cyclic discharge does not produce circular A/G proofs without explicit well-founded ordering
- [ ] **AQ13.1** answered in §Architecture Q&A
- [ ] **AQ13.2** answered in §Architecture Q&A
- [ ] **AQ13.3** answered in §Architecture Q&A
- [ ] **AQ13.4** answered in §Architecture Q&A
- [ ] **AQ13.5** answered in §Architecture Q&A

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Architecture Q&A
- **AQ13.1 — Contract as primitive vs derived**: Is `Contract` a first-class IR or a wrapper over μ-calculus formulas + metadata?
  - Answer:
  - Evidence:
  - Action:
- **AQ13.2 — Codesign in `mununu-core` vs as adapter**: Can codesign be expressed as a sequence of (contract validation + verify-orchestrator + register-map adapter) calls?
  - Answer:
  - Evidence:
  - Action:
- **AQ13.3 — Black-box discharge semantics**: Is the direction-dependent soundness of chaotic-stub reflected in the type system (`EnvContract` vs `SysContract` distinct), or a runtime convention?
  - Answer:
  - Evidence:
  - Action:
- **AQ13.4 — Composition with the rest of the verification stack**: Does contract validation reuse the μ-calculus evaluator, or have its own?
  - Answer:
  - Evidence:
  - Action:
- **AQ13.5 — Declarative environment context**: Does the contract/codesign layer let a user declare *neighbor modules* or *named interface contracts* (e.g. ready/valid, AXI-lite, ready-to-send handshakes) as first-class inputs whose guarantees bound the DUT's inputs, or must every environment assumption be hand-encoded in μ-calculus? A named-contract layer would enable a library of reusable protocol assumptions; hand-encoding forces every user to re-derive them and makes cross-project comparison harder. Check: does `codesign/` or `contract/` expose anything shaped like `interface_contract: "ready_valid"` today?
  - Answer:
  - Evidence:
  - Action:

### Decisions
-

### Follow-ups
-

---

## Chapter 14 — CLI Surface

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Each `--help` matches the handler (no orphan flags)
- [ ] At least one `--help`-documented example per subcommand actually runs
- [ ] Smoke test:
  ```bash
  mununu --help
  for cmd in context extraction sv templates library contract codesign verify memory server btor2; do
      mununu $cmd --help >/dev/null && echo "OK $cmd" || echo "FAIL $cmd"
  done
  ```

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 15 — HTTP API Surface

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Smoke test: start server, `curl /health`, `curl /templates`, POST `/context/summarize` with small CTXDSL
- [ ] **I15.1**: no summary endpoint runs synthesis (grep `handlers.rs` for `synthesize` calls in non-synthesis handlers)
- [ ] **I15.2**: every handler logs parse / realize / work phases separately via `tracing::info!` with `Instant`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 16 — UI Parity

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Every endpoint from Ch 15 has a TS client function in `mununu-ui/src/api/endpoints.ts`
- [ ] Single-surface exceptions declared inline with `surface: CLI-only — <reason>`
- [ ] Smoke test: run `/parity-check` skill OR hand-walk each endpoint to its UI caller

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 17 — Cross-Cutting Policies

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Each skill produces a clean report OR triaged exceptions filed as follow-ups
- [ ] No undocumented soundness decisions remain after triage
- [ ] Run: `/parity-check`
- [ ] Run: `/docs-traceability`
- [ ] Run: `/soundness-check`
- [ ] Run: `/review-orchestrator`

### Findings
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Chapter 17A — Architecture Validation (closing bookend)

Started:
Reviewer:
Status: not started

### Acceptance Criteria
- [ ] Every architectural claim from Chapter 1A§4 has a verdict line (survived / falsified / partial / unanswered)
- [ ] Every architecture question (1A§5 + AQ2.1–2.4 + AQ4.1–4.4 + AQ6.1–6.4 + AQ7.1–7.4 + AQ9.1–9.5 + AQ13.1–13.5) has a verdict line
- [ ] All `arch`+`bug` findings are resolved OR have a filed follow-up (RFC stub or refactor plan)
- [ ] CLAUDE.md or `docs/design/` updated with each surviving rationale
- [ ] Any theme with ≥3 findings has an RFC stub linked from this entry

### §1 Verdicts on architectural claims (from 1A §4)

| # | Claim | Verdict | Evidence (finding IDs / chapter refs) |
|---|-------|---------|---------------------------------------|
| C1 | | | |
| C2 | | | |
| C3 | | | |
| C4 | | | |
| C5 | | | |
| C6 | | | |

### §2 Verdicts on architecture questions

| # | Question | Verdict | Evidence |
|---|----------|---------|----------|
| AQ-1.1 | | | |
| AQ-1.2 | | | |
| AQ-1.3 | | | |
| AQ-1.4 | | | |
| AQ2.1 | | | |
| AQ2.2 | | | |
| AQ2.3 | | | |
| AQ2.4 | | | |
| AQ4.1 | | | |
| AQ4.2 | | | |
| AQ4.3 | | | |
| AQ4.4 | | | |
| AQ6.1 | | | |
| AQ6.2 | | | |
| AQ6.3 | | | |
| AQ6.4 | | | |
| AQ7.1 | | | |
| AQ7.2 | | | |
| AQ7.3 | | | |
| AQ7.4 | | | |
| AQ9.1 | | | |
| AQ9.2 | | | |
| AQ9.3 | | | |
| AQ9.4 | | | |
| AQ9.5 | | | |
| AQ13.1 | | | |
| AQ13.2 | | | |
| AQ13.3 | | | |
| AQ13.4 | | | |
| AQ13.5 | | | |

### §3 Themes and decisions

| Theme | Constituent finding IDs | Decision (accept / refactor / RFC) | Follow-up link |
|-------|--------------------------|-------------------------------------|----------------|

### §4 CLAUDE.md / docs/design/ updates

- [ ] Surviving rationale for each accepted claim recorded
- Diff or commit sha:

### Findings (architectural meta-findings from the validation itself)
| ID | Severity | Category | Anchor | Description | Status |
|----|----------|----------|--------|-------------|--------|

### Decisions
-

### Follow-ups
-

---

## Closing Rollup

> Fill in when all chapters are `done` (including 1A and 17A).

### `bug`-severity rollup, by category

#### `behavior` × `bug`
| ID | Anchor | Description | Resolution |
|----|--------|-------------|------------|

#### `arch` × `bug`
| ID | Anchor | Description | Resolution / RFC link |
|----|--------|-------------|------------------------|

#### `doc` × `bug`
| ID | Anchor | Description | Resolution |
|----|--------|-------------|------------|

### Architecture-validation summary (cross-ref to Ch 17A)

- Claims surviving:
- Claims falsified:
- Open RFCs:

### Onboarding doc completion check (Ch 17A exit gate)

- [ ] Every section of `ONBOARDING.md` has prose
- [ ] Every section has a `Stability:` annotation (one of stable / evolving / experimental / planned)
- [ ] Every section has a `Last reviewed:` stamp (date + chapter ref)
- [ ] §7 *Living Architecture* reflects the verdicts from Ch 17A
- [ ] CLAUDE.md updated with the onboarding-stewardship clauses (PR-template requirement + `make doc-freshness` target)
- [ ] (optional) `ShareOnboardingGuide` run to publish a shareable link; short_code recorded here:

### Review-tag commit
- Tag:
- Date:
- Reviewer:
