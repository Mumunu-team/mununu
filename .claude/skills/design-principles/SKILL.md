---
name: design-review
description: >
  Evaluates code against KISS, DRY, SOLID and YAGNI as mununu applies them:
  the three-layer architecture (extraction → adaptation → verification), the
  STS-IR waist, the engine roster and the dependency direction between the
  protected core modules, the adapters and the API. Use when asked to review
  design, architecture or code structure; invoked by review-orchestrator as
  `/design-principles`.
---

Review $ARGUMENTS (with no arguments, the branch against `main` plus the working tree, `.rs` files) for design-principle violations. Thresholds come from `.quality/thresholds.toml`; the architecture from `docs/design/engine-routing.md` and `docs/design/sts-ir.md`.

## The shape to hold the code against

- **Three layers.** Extraction (`crates/mununu-extract`, `adapter/extraction/`, the slang / yosys lift in `adapter/{slang,yosys,systemverilog}/`) produces an IR; adaptation (`adapter/ir.rs`, `adapter/emit.rs`, `adapter/sts_ir.rs`, `adapter/clts_to_ir.rs`) carries it to the engines; verification (`clts/`, `mu_calculus/`, `composition/`, `abstraction/`, the BTOR2 engines under `adapter/btor2/`, the portfolio in `adapter/reach_portfolio.rs`, routing in `planner/mod.rs`, the orchestrator in `verify/`) decides. Code that crosses a layer boundary the short way (an engine parsing source text; an adapter deciding a verdict) is the first thing to report.
- **Adapters today**: TLSF, AIGER, Promela, XState (CrewAI and LangGraph build XState JSON and delegate to it — intentional), microcode, extraction specs, BTOR2 (reader + engines), SystemVerilog via slang + yosys, VCD, Verilator (replay only), the subprocess engine drivers (btormc, pono, rIC3, cvc5), sidecars, templates, partition / cone-of-influence. Several of these are scheduled for removal by the capability lane's simplification series (SPACER, cvc5 / MathSAT / interpolation / abs_safety / refine, the `boolector` feature) — do not propose polishing code on that list; say "scheduled for removal" and move on.
- **One op layer, two transports.** `api/ops.rs` is where a verb's work lives; the clap handler in `crates/mununu-cli/src/main.rs`, the axum handler in `api/handlers.rs` and the MCP tool in `api/mcp.rs` call it. A verb implemented twice is the DRY finding that also breaks parity.

## Principles

**KISS.** Function length RED ≥ 50 / YELLOW ≥ 30 lines, nesting RED ≥ 5 / YELLOW ≥ 4, params RED ≥ 6 (from the thresholds file). An adapter `translate()` may run long if each construct arm is simple. **Known baseline giants** — `adapter/slang/verify_auto.rs` (> 15 k lines), `adapter/btor2/symbolic_bitblast.rs`, `crates/mununu-cli/src/main.rs` (> 5 k) — are over every threshold already: report them only when the change *grows* them (lines added vs removed in the diff), with the function the growth went into. A quality session owns their decomposition, not a review.

**DRY.** Look across: adapter implementations vs the shared emitter; CLI vs API vs MCP handlers vs `api/ops.rs`; test setup vs `crates/mununu-core/tests/adapters/support.rs` and `src/test_support.rs`; the eager and lazy predicate-cube paths (the 2026-06 unification keeps them differential-tested — a divergence is a finding). **False-DRY warning:** two blocks that look alike but act on different domain types (a CLTS transition vs a μ-calculus binder; a BTOR2 node vs an STS-IR term) are a YELLOW for review, not a merge proposal — a forced unification is the top source of plausible-but-wrong logic in this agent-coauthored codebase.

**SOLID.**
- *Single responsibility*: one module per format; the emitter emits; parsing and emission never share a function.
- *Open/closed*: a new engine member joins the roster in `adapter/reach_portfolio.rs` and the routing in `planner/mod.rs` without editing other members; a new adapter joins the dispatch without editing other adapters.
- *Interface segregation*: adapter traits stay minimal; no adapter implements a method it stubs.
- *Dependency inversion*: the protected modules `clts/`, `ltl/`, `mu_calculus/`, `composition/`, `context/` import nothing from `adapter/` (`grep -rn 'use crate::adapter' crates/mununu-core/src/{clts,ltl,mu_calculus,composition,context}/`); engine code imports nothing from `api/`; the planner decides routing and engines do not reach back into it.

**YAGNI.** Unused fields or variants; a generic instantiated once; a trait with one non-test impl; infrastructure for a format nobody ships; code behind a feature no CI lane enables (`grep -n features .github/workflows/*.yml` — `cargo test --workspace` enables `api` and `ast-extract` through mununu-cli's edge; `syntcomp`, `boolector`, `stress`, `dhat` are opt-in and must each have a lane or a stated reason). Abstractions introduced "for testing" when the concrete type is already constructible in tests.

**Capability use** (`CLAUDE.md` §Adapter / Emitter Capability Use) is `/soundness-check` item 6 — cross-reference, do not duplicate.

## Output format

For each violation: quote the offending code with `path:line`, name the principle, propose the concrete simpler alternative, and say whether it is new in the diff or pre-existing. End with the list of baseline giants the change grew, if any.
