---
name: rust-review
description: >
  Reviews Rust code for idiomatic patterns, ownership/borrowing correctness,
  error handling at the engine's abstention seams, hot-path allocation,
  budget and cancellation propagation across threads, and feature gating.
  Use when asked to review, audit, or check Rust code quality; also invoked
  by review-orchestrator as `/rust-best-practices`.
---

Perform a Rust best-practices review of $ARGUMENTS. With no arguments, the scope is the branch against `main` plus the working tree:

```bash
base=$(git merge-base origin/main HEAD); { git diff --name-only "$base" HEAD; git status --porcelain | awk '{print $2}'; } | grep '\.rs$' | sort -u
```

The workspace (edition 2024, virtual root manifest since 2026-11-09 — there is no root `src/`): `crates/mununu-core` (the engine, adapters, planner, HTTP + MCP API behind the `api` / `mcp` features), `crates/mununu-cli` (the `mununu` binary), `crates/mununu-extract` (tree-sitter extraction). Z3 is a mandatory dependency of mununu-core; OxiDD is the ROBDD library. Architecture: `docs/design/engine-routing.md`, `docs/design/sts-ir.md`.

## Review checklist

1. **Ownership & borrowing.** Flag needless `.clone()`, `Rc`/`RefCell` where a borrow does, `Arc<Mutex<_>>` with no documented sharing. Adapters receive `&AdapterIR` (`crates/mununu-core/src/adapter/ir.rs`) and the engines receive the STS-IR (`crates/mununu-core/src/adapter/sts_ir.rs`) — a function that takes ownership of either to read it is a finding; a module that re-derives the IR it was handed is a design finding (hand it to `/design-principles`).

2. **Error handling at the abstention seam** (`CLAUDE.md` §Failure Paths). The engine modules carry `#![deny(clippy::unwrap_used, clippy::expect_used)]` (`adapter/btor2/{symbolic_bitblast,bit_blast,native_bmc,native_interp,native_boolector,cegar,witness,bad_monitor,fault}.rs`, `adapter/{reach_portfolio,reach_rescue,fault_adversary}.rs`): a new `#[allow(clippy::unwrap_used)]` needs a comment naming the invariant; an `expect` message names an invariant the code itself established ("inserted above"), never a value a design or a tool controls. Library code propagates with `?` and `thiserror` types; the CLI may `expect` with a message at its top-level entry only. Whether a discarded error reaches the report is `/failure-path-audit`'s question — here, judge the idiom (a `match` with an `Err(_) => default` arm is the smell to point at).

3. **Idiomatic patterns.** Iterators over index loops; `Option`/`Result` combinators; `impl Trait` returns where the type is an implementation detail; `let … else` for early exits. The adapter shape is parse → IR → emit (`adapter/emit.rs` is the shared CTXDSL emitter); a new adapter that formats CTXDSL text itself duplicates it.

4. **Hot paths.** No allocation inside the fixpoint and apply loops: `clts/`, `mu_calculus/` (the evaluator and the parity-game solvers), `composition/`, `adapter/btor2/{bit_blast,symbolic_bitblast,symbolic_engine}.rs` (the ROBDD engine), `adapter/btor2/native_bmc.rs` (query construction). Labels are `SmallVec<[LabelId; 4]>` in `clts/mod.rs`; prefer `bitvec` / `smallvec` to `Vec<bool>` / short `Vec<T>`. O(n²) over states or transitions in an enumeration is a finding; a measured claim beats a guess (`CLAUDE.md` §Falsifier First: under ~2× on this host is not evidence).

5. **Budgets and cancellation across threads.** The run budget is a thread-local (`adapter/run_budget.rs`): a `thread::spawn` or scoped thread in engine code that does not capture and re-enter the caller's budget runs unbounded (the #816 class); a solver query must read the cancellation gate so an interrupted query is never read as a verdict (`QueryGate`, #879); a subprocess member is killed with everything it forked (process group on Unix, Job Object on Windows, #875/#922) — both `cfg` branches need a test.

6. **API guidelines.** snake_case / CamelCase; `into_*` consumes, `as_*` borrows cheaply; a builder only for a struct whose fields interdepend (a builder on a three-field struct is YAGNI). A new `pub` field on a struct constructed in several files (`OriginalTransition`, `TransitionSpec`, `SignalAnnotation`, `CegarOptions`, `PredicateCubeLiftOptions`, `ExactSymbolicOptions`) triggers the pre-push workspace check (`CLAUDE.md` §CI Requirements) — note it.

7. **Unsafe.** The only `unsafe` in non-test code today is the CLI's `std::env::set_var` (unsafe under edition 2024, `crates/mununu-cli/src/main.rs`). FFI goes through safe crates (z3, oxidd, tree-sitter). Any new `unsafe` is a finding unless it is an FFI boundary with a `// SAFETY:` comment; a test never sets the process environment (`env_var::read` has a thread-local override, #880/#883).

8. **Feature gates.** `axum` / `tower` / `tower-http` / `tokio` / `schemars` behind `api`; `rmcp` behind `mcp` (which implies `api`); tree-sitter behind `ast-extract`; `boolector` opt-in. An unconditional heavy dependency, or one `cargo machete` reports unused, is a finding (`cargo machete` runs in the hooks and in CI).

9. **Platform.** Paths through `std::path`, never string concatenation; `Command` arguments as separate `.arg()`s; line endings and `\r\n` tolerated in tool output parsers (the OS matrix runs the same tests on Linux, macOS and Windows).

## Output format

Group findings as **Critical** (a correctness, soundness or crash risk) / **Warning** / **Suggestion**, each with `path:line`. Do not restate a rule the finding does not cite.
