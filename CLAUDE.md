<!-- last reviewed: 2026-10-14 -->

# CLAUDE.md — Project Instructions for Claude Code

> **How to read this file.** It is loaded into every Claude Code session in this repo. Sections tagged `[RULE]` are binding — violating them blocks merge. `[REFERENCE]` and `[CONTEXT]` are background. Each rule is stated here in short form; the full text, with the incidents that produced it, lives under `docs/policies/` and is linked from the rule. When a task touches a subsystem documented under `docs/`, follow the link rather than guessing.

## Contents

1. [Project Overview](#project-overview) `[CONTEXT]`
2. [Workspace Structure](#workspace-structure) `[REFERENCE]`
3. [CI Requirements](#ci-requirements) `[RULE]`
4. [Git Identity](#git-identity) `[RULE]`
5. [Surface Parity](#surface-parity) `[RULE]`
6. [Soundness Guarantees](#soundness-guarantees) `[RULE]`
7. [Abstraction Guidelines](#abstraction-guidelines) `[RULE]`
8. [Documentation Traceability](#documentation-traceability) `[RULE]`
9. [Claims Integrity](#claims-integrity) `[RULE]`
10. [Cross-Repo Impact Reporting](#cross-repo-impact-reporting) `[RULE]`
11. [Reproducers Before Fixes](#reproducers-before-fixes) `[RULE]`
12. [Adapter / Emitter Capability Use](#adapter--emitter-capability-use) `[RULE]`
13. [Git Operations & Destructive Commands](#git-operations--destructive-commands) `[RULE]`
14. [Code Reuse, Dead Code, Performance, Security](#code-reuse-dead-code-performance-security) `[RULE]`
15. [Test the Layer the Bug Lives In](#test-the-layer-the-bug-lives-in) `[RULE]`
16. [Available Agents and Skills](#available-agents-and-skills) `[REFERENCE]`
17. [Private Files Policy](#private-files-policy) `[RULE]`
18. [Reasoning & Recommendation Honesty](#reasoning--recommendation-honesty) `[RULE]`
19. [Engine Specificity in Proposals](#engine-specificity-in-proposals) `[RULE]`
20. [Reference Docs Index](#reference-docs-index) `[REFERENCE]`
21. [Environment Variables](#environment-variables) `[REFERENCE]`

---

## Project Overview

Mununu is a formal verification tool for analyzing and synthesizing controllers for reactive systems modeled as Compositional Labeled Transition Systems (CLTS). It includes mu-calculus formula evaluation, controller synthesis, LTL pattern support, state variable abstraction, and a DSL (CTXDSL) for specifying concurrent systems. The three-layer model (extraction → adaptation → verification) and the engine roster are in `docs/design/` — start with [`docs/design/engine-routing.md`](docs/design/engine-routing.md) and [`docs/design/sts-ir.md`](docs/design/sts-ir.md).

## Workspace Structure

Cargo workspace, three crates:

```
crates/
├── mununu-core/      (lib: verification engine, adapters, composition, IR, shared types)
├── mununu-cli/       (bin: `mununu` — CLI + HTTP API server)
└── mununu-extract/   (bin: `mununu-extract` — tree-sitter AST extraction)
```

Edition 2024. Toolchain pinned by `rust-toolchain.toml`; see [`docs/toolchain.md`](docs/toolchain.md).

## CI Requirements

Full text and history: [`docs/policies/ci-and-hooks.md`](docs/policies/ci-and-hooks.md).

- **The gate is `make ci`** (`cargo fmt --check` + `cargo clippy -D warnings` + `cargo test --workspace`) — the same command locally, in the pre-commit hook (`./scripts/setup-hooks.sh`) and in GitHub Actions. A CI failure `make ci` cannot reproduce means the contract is broken. Container-exact reproduction: [`docs/dev-container.md`](docs/dev-container.md); finer recipes: [`docs/build-recipes.md`](docs/build-recipes.md).
- **The slang-gated `e2e_` tests are `#[ignore]`d** (slang / sv2v / yosys are not bundled) and run in the `mununu-sva` image; `make e2e` runs them one test per process, because an abort (stack overflow, arena exhaustion) in a shared libtest process silently skips every later test — that is how 19 failures went unnoticed (mununu#503). The image, not the nightly, is the e2e signal until the nightly has a readable history.
- **Validate any slang / SVA-touching change in the image, never on the bare host.** The host usually lacks slang, and the sv2v-only lift silently drops `assert property`, so a host `holds` on SVA is presumed vacuous until reproduced under `mununu-sva`.
- **Pre-push workspace check** when a commit touches a `pub` field of a struct with multiple construction sites (`OriginalTransition`, `TransitionSpec`, `SignalAnnotation`, `CegarOptions`, `PredicateCubeLiftOptions`, `ExactSymbolicOptions`, …): `cargo check --workspace --all-features --tests`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test --workspace --all-features --doc`. Per-crate checks miss the legacy root bin, workspace-edge crates, fixtures and doctests (three CI-red incidents).
- **Hook serialisation.** While a pre-commit hook runs, never start a second heavy cargo workload (`--workspace` anything, `make ci`, another `git commit`); per-crate `cargo check|clippy|test -p … <one test>`, `cargo fmt`, file edits and git queries are fine. Two heavy workloads on this host turned 36 tests into 1 h 32 min. The same rule applies to a timing sweep: a test binary beside it stretched a 41 s module to 2,286 s. Do not chain commits; `--no-verify` needs explicit per-commit authorisation.

## Git Identity

All commits use `Mariano Cerrutti <vscorza@gmail.com>` — pass `--author="Mariano Cerrutti <vscorza@gmail.com>"`. Never the machine's hostname-based email.

## Surface Parity

**Every user-visible capability ships on CLI *and* HTTP API in the same PR. UI parity is required only for CTXDSL-related capabilities** (revised 2026-09-10).

- **CLI**: a clap subcommand or flag in `crates/mununu-cli/src/main.rs`. **API**: a handler in `crates/mununu-core/src/api/handlers.rs` (route in `server.rs`, types in `models.rs`). **UI** (CTXDSL only): a typed client in `mununu-ui/src/api/endpoints.ts` or a hook/component.
- CTXDSL-related = the capability's *subject* is a CTXDSL model (authoring, evaluating, composing, visualising, synthesising from, importing into). RTL / SVA verbs, contract tooling and CI-gate ergonomics are CLI + API; the absence of a UI affordance for them is not drift.
- `/parity-check` verifies this. A capability on only one of CLI / API declares itself with the traceability surface tag: `surface: CLI-only — <one-line justification>`.

## Soundness Guarantees

Mununu exhaustively verifies finite-state models; its verdicts are correct **for the model**. Transfer to the real system depends on the abstraction *and* on the property's alternation structure.

**2-valued abstraction** (legacy primitives, hand-written CTXDSL, XState, microcode, agentic, …) — only the lattice corners are sound: safety + over-approximation → SOUND; liveness + under-approximation → SOUND but weak; liveness + over-approximation and safety + under-approximation → UNSOUND. Properties with **alternating fixpoints** (νμ, GR(1) response, nested obligations) collapse under a single direction — the outer ν needs a may-style upper bound and the inner μ a must-style lower bound.

**KMTS + Kleene 3-valued** (canonical for SystemVerilog / BTOR2; available to adapters that opt in) — may and must edges tracked separately, the full modal μ-calculus over `KleeneT` / `KleeneF` / `KleeneBot`; Bruns–Godefroid (CONCUR 2000) gives definite verdicts that transfer at **every** alternation depth; `KleeneBot` triggers a CEGAR refinement. Use it whenever a μ sits inside a ν (or vice versa), or for a sound liveness verdict on an over-approximation. Theory and the per-adapter migration table: [`docs/abstraction.md`](docs/abstraction.md) §"The canonical recipe", [`docs/design/kmts-theory.md`](docs/design/kmts-theory.md).

**Discipline.** Document every `eval_expr → None` choice as over- or under-approximation with a `// SOUNDNESS:` comment (`/soundness-check` flags undeclared sites); never mix directions within one model without saying so; add a soundness regression test for every new abstraction decision; prefer the KMTS path (`Transition::modality`, `state_3valued_predicates`) when emitting a CLTS for an alternating property — legacy adapters produce `Sharp`-everywhere KMTSes that carry only the 2-valued guarantees.

Strategy extraction uses signature-based selection from iteration ranks; both players have memoryless winning strategies (positional determinacy, Zielonka 1998) — memoryless on the model-checking product = finite-memory on the plant. `ControllerMode`, lasso traces, counterstrategies and the Skolem-paradigm rules: [`docs/synthesis.md`](docs/synthesis.md).

## Abstraction Guidelines

Every adapter, CTXDSL source and sidecar makes an abstraction decision. The per-subsystem recipe — which primitive to reach for, when, and what it preserves — is [`docs/abstraction.md`](docs/abstraction.md); read it before authoring an adapter, a Kripke-builder rule or a non-trivial CTXDSL source.

- Pick the **minimum** abstraction that keeps the property decidable. Declare the posture explicitly (`AbstractionType::Symbols([…])`, a comment block, a `mem { x : shared }` tag). Annotate every information-dropping decision with `// SOUNDNESS:`. Automated extraction is viable when the shape is uniform and the source carries the structure; everything else gets a parameterised CTXDSL template, not an extractor. Add a regression test for every abstraction decision (the abstract case and one concrete case in the same class).

### Lever validation — the wall-class matrix `[RULE]` (2026-07-26)

A decidability lever (a new engine, refinement source, abstraction pass or re-plan operator, and any claim that one decides a class) is validated against the **fixed wall-class set**, `crates/mununu-core/tests/wall_class_matrix.rs`, not against whichever design is handy:

```bash
cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture   # in mununu-sva
```

Add the lever's column (or run the existing ones), confirm the **soundness invariant** (no lever contradicts a case's oracle) and measure its **marginal reach**. A lever with no marginal reach on the set is not integrated without a recorded justification. A new wall class gets a class-representative case (RTL-lifted preferred). Convenience-biased testing conflates "the lever doesn't work" with "we tested the wrong class". Working notes: `.claude/plans/wall-class-lever-evaluation.md`.

### Benchmark ledger & lever-fit catalog `[RULE]` (2026-08-04)

All corpus work flows through two private, gitignored artifacts: the **benchmark-property ledger** (`.claude/plans/benchmark-property-ledger.md` — measured verdicts only, non-vacuity-gated (`EF(target != v)` dual reachable), RTL-native with the recipe recorded; honest ∅ / ⊥ rows included) and the **lever-fit shape catalog** (`.claude/plans/lever-fit-shape-catalog.md` — design shape → lever → recipe). After any corpus measurement or lever hunt, update the ledger rows (measured + backlog with its lift wall named) and extend the catalog; a lever integration also runs the matrix. The usual bottleneck is the P1.1 harness-lift track, not lever fit.

## Documentation Traceability

Full text: [`docs/policies/documentation-traceability.md`](docs/policies/documentation-traceability.md).

Every page or section under `wiki/**`, `docs/**`, `README.md`, `examples/**/README.md` and the `.claude/agents|skills` guidance that describes a feature, flag, endpoint, format, syntax or behaviour carries, near its top, an anchor to a *live* artifact reachable from CLI, API or UI:

```markdown
> Source of truth: [`<symbol or path>`](<relative path>#L<line>) — surface: CLI | API | UI | (CLI+API+UI)
```

Exempt: `> Concept: <one-line>` sections and files tagged `> Status: planning`. Dead code, deleted tests, non-default features, files outside the repo and `mununu-private/` are not acceptable anchors. `/docs-traceability` verifies every anchor; the commit that renames or removes a symbol updates its anchors. **Cadence (advisory):** after more than 10 commits on `main` without a docs touch, the next commit ships a docs delta or records a `/docs-traceability` verdict — `make docs-audit` reports the count and the areas that moved.

## Claims Integrity

Every public claim about finding bugs, verifying properties or improving the security of external systems is backed by reproducible evidence against real implementations. Models from source, not documentation; planted bugs are demos; `sv mutate` results measure the **spec's** adequacy, never a bug in the design; severity honesty (vulnerability vs correctness vs structural gap vs pattern violation); a reproduction path or an explicit "structural, not yet reproduced"; the CTXDSL model + `mununu context eval` is the oracle, not human reading; RTL counterexamples are validated under Verilator in the `mununu-sva` image, model-only traces are labelled as such; hand-authored LLVM IR fixtures demonstrate matcher behaviour, not end-to-end clang handling; publications lead with the example, not the feature. Full policy: [`docs/policies/claims-integrity.md`](docs/policies/claims-integrity.md).

## Cross-Repo Impact Reporting

Every PR that changes user-visible behaviour of a surface a downstream repo consumes (rosf, monono, mununu-ui, …) ships **one consumer briefing** under [`docs/consumer-briefings/`](docs/consumer-briefings/) (`YYYY-MM-<slug>.md`): audience banner, TL;DR, per-consumer section, a Docker-rebuild table with no blanks, provenance, "not covered here". It fires on verdict-value or verdict-message changes, JSON shape changes, CLI flag / HTTP route adds, removes or renames, verdict-semantics changes (bugfixes count), subprocess-tool bumps and sidecar-schema changes. Internal refactors, test-only and clarifying-doc changes are exempt. Full policy: [`docs/policies/cross-repo-impact.md`](docs/policies/cross-repo-impact.md).

## Reproducers Before Fixes

Full text and incidents: [`docs/policies/reproducers-before-fixes.md`](docs/policies/reproducers-before-fixes.md).

A change that claims to fix a defect declares, in a commit trailer, exactly one of `Repro: in-repo <test>` · `Repro: consumer-only <who> — <why not here>` · `Repro: none — containment only`. Containment may not `close` the cause issue. A reproducer is a test that **fails on the parent commit and passes on this one** — a regression test that passes on the parent is decoration. `make verify-repro TEST=<substring> [REF=HEAD~1]` says `CONFIRMED`, `FAIL — PASSES at REF`, or `INCONCLUSIVE` (the test needs API this commit introduced — not a pass; it also needs a commit to diff). Before using a measurement as evidence, ask what it would have shown if the hypothesis were false; if "the same thing", it is not evidence.

## Adapter / Emitter Capability Use

Prefer the CLTS / CTXDSL primitives over re-encoding source features as state-name suffixes or parallel single-label edges:

- **Multi-label transitions are a SYNCHRONISATION VECTOR, not alternation** (mununu#570): `transition s -> t on label a, label b;` fires on `{a, b}` together (`crates/mununu-core/src/clts/mod.rs`, `SmallVec<[LabelId; 4]>`), and under composition pairs only with a set-identical partner — an unmatched compound action freezes the automaton (a 7-automaton model collapsed to one reachable state, a safety property holding vacuously). "Any one of these" is `on label a | label b`; prefer `on {label a, label b}` when you mean one joint action. A vector's partners may sit on different automata (`CompositionOptions::sync_vectors`, mununu#589). See [`docs/abstraction.md`](docs/abstraction.md) §"Multi-label transitions are synchronisation vectors".
- **Per-state predicates** (`state_variable_bitset`, `state_valuation`; CTXDSL `predicates { predicate foo = state S1; }`) instead of attributes in state names. **Per-state valuations** (`state S1 { valuations { … } };`) round-trip through the emitter.
- **Per-label controllability** (`LabelControllability::{Controllable, Internal, Uncontrollable}`) in the `controllable { … }` / `internal { … }` blocks, not in label-name prefixes.
- **Rich modal guards** (`Guard { labels, current, next, control, max_steps }`): `[(labels = {a}, req_next = {active}, ctrl = controllable)] φ` — reach for `req_next` / `forb_next` whenever the property says "after this transition the system must be in a state where …".

Reference implementations: `crates/mununu-core/src/adapter/emit.rs` (turn-aware `[(ctrl=Controllable)]` guards), `crates/mununu-core/src/adapter/systemverilog/kripke.rs` (Kripke valuations). When a primitive is intentionally unused, leave a one-line comment saying why; `/soundness-check` flags silent under-use.

## Git Operations & Destructive Commands

**Destructive git operations require explicit user instruction in the current session prompt:** `git reset --hard`, `git push --force` (any variant), `git checkout -- <paths>` / `git restore <paths>`, `git clean -f`, `git branch -D` of unmerged work, `git stash drop|clear`, interactive rebases that squash or drop unpushed work. Working-tree state is not in the reflog; a botched revert plus `--hard` reset once cost ~1,730 lines that no local-history snapshot recovered.

Prefer `--abort` / `git reset --soft`; stash with `-u -m "<reason>"` before any routine operation that must discard a tree, and surface the stash; on a hook failure make a **new** commit — never `--amend`, never `--no-verify` without authorisation; before touching files the user is editing, push a savepoint branch. Subagents inherit this; destructive actions only inside `isolation: "worktree"` agents.

## Code Reuse, Dead Code, Performance, Security

- Check for existing functionality before writing utility code; prefer established libraries, pinned with ranges. Remove unused code and dependencies promptly (`cargo clippy`, `cargo audit`); no "just in case" packages.
- Test names describe behaviour, not implementation. Prefer integration tests over excessive mocking — except at an internal seam, where [Test the Layer the Bug Lives In](#test-the-layer-the-bug-lives-in) applies.
- **API handlers** re-parse and re-realize from scratch — keep post-realization work light; never run controller synthesis in a summary endpoint; add `tracing::info!` timing per phase; the UI client times out at 10 s (120 s extended).
- **Formula inversion**: do not negate fixpoint variable references inside the body — the dual fixpoint's changed starting point handles the semantics; negating them oscillates forever.
- **Wiki** pages (`wiki/`) are updated when DSL syntax, endpoints, UI flow, composition modes or formula operators change; every CTXDSL example in them is tested against the binary before publishing.
- **Security (OWASP)**: never interpolate user input into commands or templates; validate external input; no sensitive data in logs.

## Test the Layer the Bug Lives In

When a change teaches one layer to produce something new, add a test **at the seam** where the next layer consumes it, in the cheapest tier that can express the question. An end-to-end test *detects* a class of defect; it is never the *home* of one. (mununu#565: a new translator atom shape passed eighteen unit tests and `make ci`, and only a ten-minute `#[ignore]`d e2e caught it — the defect was in a pure classifier the same file tested eight ways in 0.00 s; what was missing was a table from "the translator can emit shape X" to "the consumer accepts shape X".)

- **Find the pure function** — a classifier, a lowering, a normaliser; `every_translator_atom_shape_has_a_known_seeding_classification` in `adapter/slang/verify_auto.rs` is the worked example.
- **Keep the e2e** as the detector and the cross-check that the cheap tier is faithful.
- **Derive the mock from the real call site; never invent it.** When the cheap tier and the e2e disagree, that disagreement is a finding — record it and measure, rather than asserting the answer you expected.

## Available Agents and Skills

Under `.claude/skills/**` (invoke as `/<name>`) and `.claude/agents/**` (spawn by name). Invoke them rather than reproducing their logic inline.

| Name | When to invoke |
|---|---|
| `/parity-check` | Verifies a capability is wired across CLI + API (+ UI for CTXDSL). Run before calling a feature "done". |
| `/docs-traceability` | Walks the covered Markdown and verifies every `Source of truth:` anchor. |
| `/soundness-check` | Flags undocumented `eval_expr → None` choices and silent under-use of CLTS / CTXDSL primitives. |
| `/design-principles` | KISS / DRY / SOLID / YAGNI review of a target scope. |
| `/rust-best-practices` | Idiomatic-Rust review: ownership, error handling, API shape. |
| `/security-audit` | Vulnerabilities, `unsafe`, input handling. |
| `/test-coverage` | Coverage, test quality, missing scenarios. |
| `/quality-inventory` | One-shot SLOC / pub-count / nesting / dead-code inventory. |
| `quality-session` | Metric-driven refactoring session with before/after measurement (not a point-in-time review). |
| `review-orchestrator` | Runs the review skills and produces one consolidated report. |
| `domain-adequacy` | Checks a domain profile (agentic, codesign) covers its controllability / composition / label conventions. |
| `sidecar-auditor` | Audits every sidecar implementation for redundancy and design; analysis and a phased plan, no edits. |
| `verification-prospector` | Mines literature and corpora for verification targets; maintains the backlog. |
| `target-executor` | Takes one backlog target end to end (spec / sidecar / CTXDSL, run, report); its Phase 3.5 is the Verilator-under-`mununu-sva` counterexample procedure. |

When extending one, its `description:` and inline guidance follow Documentation Traceability.

## Private Files Policy

Sensitive or unpublished material lives in `/Users/marianocerrutti/git_repo/mununu-private/`, never here: paper sources, benchmark scripts and expected outputs, extraction specs with CVE data, protocol CTXDSL models, MCP-extracted CTXDSL, governance scripts, tutorials, evaluation data and drafts. Public: Rust source, adapters and their formats, the extraction framework's *capabilities* (adapter code, validation, schema), shared IR, tests, README and wiki. Before adding a file here you would not want public, move it to `mununu-private` and add the path to `.gitignore` (which already excludes `/paper/`, `/artifact/`, `/examples/syntcomp/`, `/examples/scalable/`, `/tutorial/`).

## Reasoning & Recommendation Honesty

Never justify a decision, recommendation or deferral with a limitation the agent does not have — "the session is long", "end of session", fatigue, attention erosion. Turn N is produced at the same capability as turn 1. The genuine session-linked factors, named when they apply: **context degradation** (earlier invariants can be summarised away — which argues for doing dependent work *while* the context is warm, not deferring it), **token / compute cost** (the user's axis to weigh — surface it, do not assume it as a stop), **intrinsic difficulty / soundness-criticality** (answered with procedural rigour — differential tests, oracles — not postponement). If the only reason is "the user should make this call", say exactly that. Warm build caches typically make longer-running work cheaper, not riskier.

## Engine Specificity in Proposals

Every proposed experiment, approach, design or implementation states which engine(s) it uses and how — never "the verifier decides it". Name the engine (`exact-symbolic` ROBDD / OxiDD · `symbolic` / `explicit` predicate-cube KMTS · native BMC / k-induction · McMillan interpolation · SPACER · btormc / pono · ranking certificate), its structure (full-state ROBDD · predicate cube + may/must · QF_BV / QF_AUFBV · CHC), technique (bit-blast μ-fixpoint · 3-valued modal-μ + CEGAR · k-induction · CEG-prophecy · well-founded ranking), configuration (bit cap, theory, must-edge inference, budgets, owned vs subprocess) and role (decider · differential oracle · cross-check). One `engine:` line per step. It makes the soundness posture and cost checkable at a glance and ties the proposal to the roster in [`docs/design/engine-routing.md`](docs/design/engine-routing.md).

## Reference Docs Index

- [`docs/env-vars.md`](docs/env-vars.md) — every `MUNUNU_*` variable the code reads, one line each.
- [`docs/policies/`](docs/policies/) — the full policies: `ci-and-hooks.md`, `reproducers-before-fixes.md`, `documentation-traceability.md`, `claims-integrity.md`, `cross-repo-impact.md`.
- [`docs/dev-container.md`](docs/dev-container.md) · [`docs/build-recipes.md`](docs/build-recipes.md) · [`docs/toolchain.md`](docs/toolchain.md) · [`docs/docker.md`](docs/docker.md) — building and running.
- [`docs/cli-cookbook.md`](docs/cli-cookbook.md) — common `mununu` invocations.
- [`docs/ctxdsl-modelling-guide.md`](docs/ctxdsl-modelling-guide.md) — hand-authoring CTXDSL: the one-comparison `guard` fragment (`&&` silently disables the transition), pre-state `effects`, where `var == value` binds vs silently evaluates true. Read before writing a model with `variables`.
- [`docs/verifying-rtl.md`](docs/verifying-rtl.md) — the property verbs (`btor2 verify` / `verify-liveness` / `verify-recoverability` / `verify-justice`), no-sidecar `sv verify-auto`, inputs-in-the-state, the diagnostics, the agent-over-HTTP path, and **"What decides at what width"** — the exact engine's measured per-shape frontier under the shipped defaults (`scripts/bitcap_frontier.py`, EXP-0031). The cap is 40 bits by default, auto-raised to fit the cone up to 192; within it the cone's SHAPE decides, so never quote "~40 bits" as the engine's reach.
- [`docs/synthesis.md`](docs/synthesis.md) — `ControllerMode`, strategy extraction, lasso traces, Skolem-paradigm rules.
- [`docs/abstraction.md`](docs/abstraction.md) — the per-subsystem abstraction recipe.
- [`docs/adapters/agentic.md`](docs/adapters/agentic.md) · [`wiki/Agentic-Adapters.md`](wiki/Agentic-Adapters.md) · [`docs/adapters/extraction.md`](docs/adapters/extraction.md) · [`docs/adapters/tlsf-aiger.md`](docs/adapters/tlsf-aiger.md) · [`wiki/Verify-Project-Flow.md`](wiki/Verify-Project-Flow.md) — adapters and the N-source verify framework.
- [`docs/api-schemas/`](docs/api-schemas/) — the stable JSON wire types; the drift test `api_schema_drift_*` fails CI on any shape change without a schema update (`MUNUNU_UPDATE_API_SCHEMAS=1` regenerates).
- [`docs/design/`](docs/design/) — design notes: `kmts-theory.md`, `engine-routing.md`, `sts-ir.md`, `bdd-variable-ordering.md` (read before touching the exact engine's order or reading a peak), `combinational-input-atoms.md`, `past-shadow-soundness.md`, `predicate-image-soundness.md`, `antecedent-shadow-synthesis.md` (superseded 2026-10-14 by inputs-in-the-state, mununu#677 — kept as history), and the rest.

## Environment Variables

The full table is [`docs/env-vars.md`](docs/env-vars.md). Three to know before reading a result:

| variable | why it matters |
|---|---|
| `MUNUNU_BDD_TIME_BUDGET_MS` | off by default (`0`): a wall clock makes the **verdict** host-dependent, and that ⊥ is indistinguishable from a real one. |
| `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` | a SIGKILL-survivable breadcrumb, **not** authoritative for a portfolio run — the planner's merge writes nothing, so a `main`-phase `unknown` can end the run decided. |
| `MUNUNU_SHADOW_BTOR2_DUMP` | the BTOR2 the engine runs, **excluding** the inputs-in-the-state lift (per property, inside the engine); the header says so. |
