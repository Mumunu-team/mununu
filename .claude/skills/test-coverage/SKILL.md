---
name: test-review
description: >
  Audits test coverage, test quality and missing scenarios against mununu's
  test tiers: the unit and per-area integration binaries in `make ci`, the
  proptest tier, the `#[ignore]`d `e2e_` suite that runs only in the
  `mununu-sva` image, the reproducer rule, the seam-test rule and the
  negative-control rule. Use when asked to review tests or check coverage;
  invoked by review-orchestrator as `/test-coverage`.
---

Audit test coverage for $ARGUMENTS (with no arguments, the branch against `main` plus the working tree). Read `CLAUDE.md` §CI Requirements, §Reproducers Before Fixes and §Test the Layer the Bug Lives In first; the full policies are `docs/policies/ci-and-hooks.md` and `docs/policies/reproducers-before-fixes.md`.

## The tiers as they exist

| Tier | Where | Runs in |
|---|---|---|
| Unit | `#[cfg(test)]` modules in `crates/*/src/**` | `make ci` (`cargo test --workspace`, with the test-count gate: `make test` refuses to pass unless its summary shows ≥ the floor in `measurements/test-count-floor.txt`) |
| Integration, per area | `crates/mununu-core/tests/{adapters,ctxdsl,engine,properties}/main.rs` + `mod` files — **a new integration test joins one of these**; a new top-level `tests/*.rs` is a new link of mununu-core (the CI lever N0.4 removed); the accepted exception is a single-issue reproducer named `<topic>_<issue>.rs` | `make ci` |
| Property | `crates/mununu-core/tests/properties.rs` (`proptest`), `make test-properties`, `make proptest-deep` | `make ci` (shallow) |
| Doctests | `cargo test --workspace --doc` | pre-push hook, CI |
| CLI | `crates/mununu-cli/tests/cli_session.rs` (`assert_cmd` + `predicates`) | `make ci` |
| Benchmarks | `crates/mununu-core/benches/*.rs` (criterion) | `make bench-*`, never a gate |
| e2e | `#[ignore]`d tests whose names start `e2e_`, plus `tests/wall_class_matrix.rs` and the ledger (`measurements/ledger/verdicts.csv`, `make ledger-check`) | **only** `make e2e` inside the `mununu-sva` image, one process per test (mununu#504); a host run is not evidence |
| OS matrix | `.github/workflows/os-matrix.yml` — the same tests on Linux, macOS, Windows | CI; `measurements/os-matrix-unix-only-tests.txt` must stay empty |

## Review checklist

1. **Module coverage.** Every `.rs` under `adapter/`, `context_dsl/`, `mu_calculus/`, `planner/`, `verify/` has a `#[cfg(test)]` module or a named integration test that exercises it; list the files with neither.

2. **Three-level adapter testing.** Per construct (parse → IR → emission), multi-construct with a reference verdict (SPIN, Strix, ABC, btormc, the exact engine as oracle), and regression fixtures (`crates/mununu-core/tests/fixtures/`, `adapter/*/testdata/`); for RTL additionally one `e2e_` in the image. Name the missing level.

3. **Reproducers before fixes.** A change whose subject says `fix` carries a test that **fails on the parent commit and passes on this one**, and a `Repro:` trailer. Check: the test uses only APIs the parent has (otherwise `make verify-repro` reports `INCONCLUSIVE`, which is not a pass — the #929 reproducer's comment is the pattern); `make verify-repro TEST=<substring> REF=<parent>` says `CONFIRMED`. A regression test that passes on the parent is decoration; say so.

4. **Test the layer the bug lives in.** When the change teaches one layer to produce a new shape, there is a test at the seam where the next layer consumes it, in the cheapest tier that can express it — a table from "the producer can emit X" to "the consumer accepts X" (`every_translator_atom_shape_has_a_known_seeding_classification` in `crates/mununu-core/src/adapter/slang/verify_auto.rs`). The e2e stays the detector; it is never the only home. When the cheap tier and the e2e disagree, that is a finding to record, not a test to adjust.

5. **Soundness regression per abstraction decision.** A new `// SOUNDNESS:` site or abstraction default ships the abstract case and one concrete case in the same class (`CLAUDE.md` §Abstraction Guidelines).

6. **Negative controls.** A gate ships with the input that makes it fail, run in the same command (`scripts/test_count_gate.sh --control`, `scripts/tracker_audit.sh --control`, `the_scan_catches_plans_links_and_ignored_roots_only` in the G4 test, `MUNUNU_PREPUSH_SELFTEST=1 scripts/pre-push`). A new check without one is a finding (Track C: a check that cannot fail where it is pointed).

7. **Test quality.** Flag `assert!(x.is_ok())`-only and "no panic" tests; require the verdict, count or emitted text to be asserted; error paths (malformed input, missing field, unknown construct) tested. **Load independence**: no `thread::sleep` as a timing assertion, no wall-clock bound a loaded host can miss (the 2026-11-07/08 fixes: a 2 s member timeout with a polled bound, a deterministic z3 `rlimit` instead of a wall), no test sets the process memory ceiling (#871), no test writes the process environment (`env_var::read`'s thread-local override, #880/#883), no `#[ignore]` without a reason naming what it needs.

8. **Property tests.** Suggest `proptest` for parsers (CTXDSL, μ-calculus, BTOR2 round-trips), BDD algebra, guard evaluation equivalences, and the shared random-BTOR2 generator (N0.6) for engine differentials — exact vs k-induction vs IC3 vs the cube engine — as the seconds-long tier in `make ci`.

9. **Edge cases.** Empty inputs, one state, self-loops, the bit-cap boundary (40 / 192 / 1024), widths over 128 bits (`u128` arithmetic on a 135-bit cell was mununu#848), Unicode identifiers.

10. **CI alignment.** The test runs where the reviewer thinks it does: `make ci` = `cargo fmt --check` + `cargo clippy -D warnings` + `cargo test --workspace` (the `api` tests are included through mununu-cli's dependency edge); anything `#[ignore]`d runs only in the image; a `pub` field change on a multi-construction-site struct needs the `--all-features --tests` workspace check; the Windows lane runs the same tests.

## Output format

Coverage gaps by module, priority-ordered, each with the concrete test to add (tier, binary or module it joins, the assertion). Then: reproducer verdict for each fix commit; seam tests missing; negative controls missing; load-dependence findings with `path:line`.
