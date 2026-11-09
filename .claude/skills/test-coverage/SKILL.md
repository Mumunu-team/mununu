---
name: test-review
description: >
  Audits test coverage, test quality, and missing test scenarios.
  Checks the three-level adapter testing requirement.
  Use when asked to review tests or check coverage.
---

Audit test coverage for $ARGUMENTS (or the full workspace if no args).

## Testing Infrastructure

- **Unit tests**: `#[cfg(test)]` modules in source files
- **Integration tests**: `crates/mununu-core/tests/` and `tests/`
- **Property tests**: `proptest` (should be used for pure functions with wide input domains)
- **Benchmarks**: `criterion` in `benches/`
- **CLI integration**: `assert_cmd` + `predicates` in `crates/mununu-cli/tests/cli_session.rs`

## Review Checklist

1. **Module coverage**: check each `src/` file for a `#[cfg(test)]` module. Flag files with none, especially in:
   - `adapter/` (every adapter MUST have tests)
   - `context_dsl/` (parser coverage)
   - `ltl/` and `mu_calculus/` (formula evaluation)

2. **Adapter three-level testing** (per CLAUDE.md governance):
   - **Unit tests (per-construct)**: each syntactic construct has a dedicated test verifying parse -> IR -> CTXDSL emission
   - **Integration tests (multi-construct)**: complete models with known verification verdicts from reference tools (SPIN, Strix, ABC)
   - **Regression tests (end-to-end)**: full source files translated, output stored in `tests/<format>/expected/`
   - Flag any adapter missing a level.

3. **Test quality**:
   - Flag tests that only assert `!= panic` or just `assert!(result.is_ok())`
   - Check for meaningful assertions on output content, state counts, realizability verdicts
   - Verify error-path tests exist (malformed input, missing fields, invalid syntax)

4. **Property tests**: suggest `proptest` for:
   - LTL formula parsing (round-trip: parse -> format -> parse)
   - BDD operations (commutativity, associativity)
   - Guard expression evaluation (equivalence of different representations)

5. **Edge cases**:
   - Empty inputs, single-state automata, self-loops
   - Maximum-size inputs (scalability boundary)
   - Unicode in identifiers and labels

6. **CI alignment**: verify tests match what CI runs (`cargo test`, `cargo test --features api`, `cargo test --features syntcomp`).

## Output Format

Coverage gaps by module, priority-ordered. Include specific test cases to add.
