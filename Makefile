# Canonical entry point for build/test/lint/verify on the mununu workspace.
#
# These verbs are the same whether you run them on the host or inside the
# `mununu-dev` Docker image. CI invokes the docker form; contributors are
# free to use either.
#
# Image: docker build -f docker/Dockerfile.dev -t mununu-dev .
# Run:   docker run --rm -v $(pwd):/work mununu-dev make <verb>

CARGO       ?= cargo
VERIFY_FILE ?= examples/hw/handshake.ctxdsl

# Reproducible-experiments scaffold (see notebook/0000-overview.md and the
# per-experiment archives under experiments/EXP-NNNN-<slug>/).
EXP        ?=
BASELINE   ?= main
PROPTEST_CASES ?= 64
DIFF_CASES ?= 512

.PHONY: build test lint verify ci clean help \
        test-fast e2e e2e-oneshot e2e-parallel ledger-check test-properties stress fuzz proptest-deep \
        diff-tier-explore \
        coverage mem-profile sweep \
        bench-baseline bench-compare bench-record \
        experiment replay publish-prep frontier-smoke \
        verify-repro docs-audit tracker-audit test-count-gate

help:
	@echo "Build / test / lint:"
	@echo "  build          - cargo build --release for mununu-cli and mununu-extract"
	@echo "  test           - cargo test --workspace, then the test-count gate (C3)"
	@echo "  test-count-gate - the gate's negative control + the last make-test.log against the floor"
	@echo "  test-fast      - cargo test --lib + integration tests, sub-minute (pre-commit gate)"
	@echo "  tracker-audit  - open issues that a commit on main claims to close (non-zero when any)"
	@echo "  e2e            - SVA-verification e2e suite, one test per process (#[ignore]d; run inside the mununu-sva image)"
	@echo "  e2e-oneshot    - same suite in a single libtest process (an aborting test loses the run)"
	@echo "  e2e-parallel   - same suite under cargo-nextest, one process per test, E2E_JOBS in flight"
	@echo "  ledger-check   - [LEDGER_ARGS=\"--write|--all|--retired E|--only D\"] re-run only the ledger rows whose cone changed (measurements/ledger/README.md)"
	@echo "  lint           - cargo fmt --check && cargo clippy -D warnings"
	@echo "  verify         - cargo run mununu against $(VERIFY_FILE)"
	@echo "  ci             - lint + test (the gate)"
	@echo "  verify-repro   - TEST=<name> [REF=HEAD~1] ; assert a defect reproducer FAILS at REF (CLAUDE.md: Reproducers Before Fixes)"
	@echo "  clean          - cargo clean"
	@echo
	@echo "Properties / stress / fuzz:"
	@echo "  test-properties - run proptest suite (PROPTEST_CASES=$(PROPTEST_CASES))"
	@echo "  proptest-deep   - run proptest suite with PROPTEST_CASES=4096"
	@echo "  diff-tier-explore - random-BTOR2 differential tier on FRESH draws (off the gate; DIFF_CASES=$(DIFF_CASES))"
	@echo "  stress          - run #[ignore]+stress feature tests (tier 3)"
	@echo "  fuzz            - run cargo-fuzz harnesses for 5 minutes each"
	@echo
	@echo "Coverage / memory:"
	@echo "  coverage        - cargo-llvm-cov HTML + JSON summary"
	@echo "  mem-profile     - run dhat-instrumented stress tests"
	@echo
	@echo "Disk hygiene:"
	@echo "  sweep           - cargo sweep on mununu's siblings under ~/git_repo/ (dry-run; SWEEP_APPLY=1 to delete)"
	@echo
	@echo "Benchmarks:"
	@echo "  bench-baseline  - cargo bench --save-baseline $(BASELINE)"
	@echo "  bench-compare   - cargo bench --baseline $(BASELINE) and run scripts/bench_diff.sh"
	@echo "  bench-record    - EXP=<EXP-ID> -- <bench-args> ; archive criterion + manifest"
	@echo
	@echo "Experiments:"
	@echo "  experiment      - EXP=NNNN-<slug> ; scaffolds experiments/EXP-<EXP>/"
	@echo "  replay          - EXP=<EXP-ID> ; replays an archived experiment"
	@echo "  publish-prep    - validate every experiment archive (scripts/check_repro.sh)"
	@echo "  frontier-smoke  - build the release binary and assert the exact engine's known bit-cap frontier points (scripts/bitcap_frontier.py smoke, ≈ 1–2 min; not in ci)"

build:
	$(CARGO) build --release -p mununu-cli
	$(CARGO) build --release -p mununu-extract

# Test-runner selection.
#
# Linux (CI's dev image): prefer cargo-nextest. nextest is faster there
# and refuses to silently drop a test binary that crashes without
# reporting; doctests run separately because nextest skips them.
#
# macOS: use `cargo test --workspace` instead. nextest spawns one process
# PER TEST, and macOS Gatekeeper runs a synchronous security assessment on
# the first exec of every freshly-built (unsigned) test binary — measured
# ~1–3 s wall / ~0 s CPU, the process parked in kernel state `U`. Paid
# thousands of times, and starved when the hook runs backgrounded at
# lowered priority, that wedges for hours (the 2026-05-27 R.2.5 incident
# and the 2026-06-07 recurrence). `cargo test` runs all tests as threads
# inside ONE process per test-binary, paying the tax once per binary —
# measured ~25× faster locally (1391 mununu-core tests in 4.9 s vs.
# nextest taking 1:59 for just 9). It also covers unit + doc tests in one
# shot. Override the auto-detection with MUNUNU_TEST_RUNNER=nextest|cargo.
# Roadmap C3 — the suite's own summary is checked against measurements/test-count-floor.txt by
# scripts/test_count_gate.sh, after its negative control; a lane that runs nothing is red, not
# green (the C1 incident: a bare `cargo test --lib api` ran 0 tests and passed for weeks).
# Runner choice (MUNUNU_TEST_RUNNER=nextest|cargo, else cargo on macOS / nextest on Linux) and
# the doctest pass live in the script, unchanged.
test:
	scripts/test_with_gate.sh

# The gate alone: its control, then the last `make test` log if there is one.
test-count-gate:
	scripts/test_count_gate.sh --control
	@log="$${MUNUNU_TEST_LOG:-$${CARGO_TARGET_DIR:-target}/make-test.log}"; \
	if [ -f "$$log" ]; then scripts/test_count_gate.sh "$$log"; else echo "test-count-gate: no make-test.log yet (run make test)"; fi

# tier-1 + tier-2: pre-commit gate. Skips #[ignore]'d stress tests by default.
# Eventually this will be the canonical pre-commit form; for now it is an
# alias to `test` until tier-3 stress tests are split out.
test-fast:
	$(CARGO) test --workspace --lib --bins --tests --examples

# End-to-end SVA-verification suite — the `#[ignore]`d tests that shell out to
# the external SystemVerilog toolchain (slang + yosys with read_slang — sv2v only as
# the dev-only differential oracle of test_support builds — plus btormc for
# the reachability differential and Verilator for the counterexample-replay
# gate). They are `#[ignore]`d in `make ci` because the tools are contributor-
# installed, not bundled (subprocess-tools-are-not-bundled policy), so this
# target runs ONLY inside the `mununu-sva` image, which pins the whole chain.
# See CLAUDE.md §"SVA-verification e2e validation (slang)".
#
#   docker build -f docker/Dockerfile.dev -t mununu-dev .   # once
#   docker build -f docker/Dockerfile.sva -t mununu-sva .   # once
#   docker volume create mununu-target                      # warm cargo cache
#   docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target \
#     mununu-sva make e2e
#
# Covers the differential-oracle suite (P1 exact↔btormc reachability, the
# corpus monotone-verdict ledger + census, the cegar↔symbolic↔exact parity
# gate, the portfolio e2e, the Verilator counterexample-replay gate) plus the
# adapter's `e2e_`-prefixed lib tests (verify-auto / extract-sva on real RTL).
# mununu#504 — this runs ONE TEST PER PROCESS via scripts/e2e-sweep.sh. A test
# that ABORTS (stack overflow, or a panic while unwinding an exhausted BDD
# arena) does not unwind, so in a single shared libtest process it kills the
# run: no summary, no failure list, and every later test silently skipped.
# That is how the set drifted to 19 unnoticed failures (mununu#503). Isolated,
# an abort becomes one CRASH row and the sweep still completes.
#
# `make e2e-oneshot` keeps the old single-process form for a quick local run.
#
# N0.2 (2026-11-03) — the suite reuses lifts: `MUNUNU_LIFT_CACHE_DIR` names a content-
# addressed cache of the SV → BTOR2 lift's raw tool outputs (adapter/yosys/lift_cache.rs),
# keyed by everything the tools are told. Under the cargo target dir, which in the image is
# the persistent volume, so it survives sweeps. Only these three targets set it; `make ci`
# lifts nothing and stays as it was. `MUNUNU_LIFT_CACHE_DIR= make e2e` runs every lift.
e2e e2e-oneshot e2e-parallel: export MUNUNU_LIFT_CACHE_DIR ?= $(or $(CARGO_TARGET_DIR),target)/lift-cache
e2e:
	./scripts/e2e-sweep.sh

# Each binary of scripts/e2e-binaries.txt (`lib` = the unit tests), with its feature args.
e2e-oneshot:
	@grep -v -e '^#' -e '^ *$$' scripts/e2e-binaries.txt | while read -r t f; do \
	  sel=$$([ "$$t" = lib ] && echo --lib || echo "--test $$t"); \
	  $(CARGO) test -p mununu-core $$sel $${f:---all-features} e2e_ -- --ignored --nocapture || exit 1; \
	done

# N0.3 (2026-11-03) — the same suite under cargo-nextest: one process per test, which is
# exactly the isolation scripts/e2e-sweep.sh builds by hand (an abort is one failed row,
# never a lost run), plus parallel scheduling. The sweep is serial by design (mununu#504)
# and took 112 min on 2026-10-26, 98 of them in four OpenTitan full-IP tests. Those are each
# an internally parallel portfolio under wall-clock row budgets: run beside each other they
# slow 4–14× and their budget-bound pins move (2026-11-04, E2E_JOBS=4: 234 min, one pinned
# count off), so `.config/nextest.toml` gives each of them every test thread — they run one
# after another, alone — and only the short tail (73 tests, 612 s in all) shares the threads.
# Needs cargo-nextest (the dev image carries it; `cargo install --locked cargo-nextest`).
# `--no-fail-fast` keeps every row; a crashed test is reported with its signal. Output is
# captured per test (nextest's `--no-capture` would run the tests SERIALLY) and a failing
# test's output is printed as it fails and again in the final summary. The two binaries are
# the sweep's, from scripts/e2e-binaries.txt: one nextest run per binary with that binary's
# feature args (`--all-features` on the `adapters` group would build its syntcomp module, which
# includes `mununu-private` fixtures at compile time and does not build in the image). Every
# binary runs even after one fails; the target fails if any did.
E2E_JOBS ?= 4
e2e-parallel:
	@grep -v -e '^#' -e '^ *$$' scripts/e2e-binaries.txt | { rc=0; while read -r t f; do \
	  sel=$$([ "$$t" = lib ] && echo --lib || echo "--test $$t"); \
	  $(CARGO) nextest run -p mununu-core $${f:---all-features} $$sel \
	    --run-ignored ignored-only -E 'test(/e2e_/)' --test-threads $(E2E_JOBS) --no-fail-fast \
	    --failure-output immediate-final --success-output never || rc=1; \
	done; exit $$rc; }

# N0.1 (2026-11-03) — the executable ledger (measurements/ledger/README.md): every design's rows
# get their cone hash for the price of a lift; only NEW / CHANGED / RETIRED rows re-run, under
# 3× their recorded time. `LEDGER_ARGS="--write"` records; `--all` is the breadth run;
# `--retired spacer` re-runs the rows a removed member decided. Needs `make build` (release
# binary) and the mununu-sva image.
ledger-check:
	@scripts/ledger_check.py $(LEDGER_ARGS)

test-properties:
# The `properties*` crates are gated on `test_support` (their generators live behind it);
# without the feature the target compiled them out and ran ZERO tests — measured 2026-11-03,
# a gate with no negative control. The feature is now passed explicitly.
	PROPTEST_CASES=$(PROPTEST_CASES) $(CARGO) test -p mununu-core --features test_support --test 'properties*' -- --nocapture

proptest-deep:
	$(MAKE) test-properties PROPTEST_CASES=4096

# The random-BTOR2 differential tier on fresh draws. `make ci` replays one fixed draw (a blocking
# gate must be deterministic); this is the explorer that looks for new engine disagreements. A
# failure prints the design: file it, pin it as a deterministic test, fix the engine.
diff-tier-explore:
	MUNUNU_DIFF_TIER_EXPLORE=1 PROPTEST_CASES=$(DIFF_CASES) $(CARGO) test -p mununu-core --test engine btor2_differential -- --nocapture

stress:
	$(CARGO) test --workspace --features stress --tests 'stress_*' -- --ignored --nocapture

fuzz:
	@if [ ! -d fuzz ]; then echo "fuzz/ not initialized; run: cargo fuzz init"; exit 1; fi
	@for target in $$(cargo fuzz list 2>/dev/null); do \
		echo "==> fuzzing $$target for 5 minutes"; \
		cargo fuzz run $$target -- -max_total_time=300 || exit 1; \
	done

coverage:
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { echo "install: cargo install cargo-llvm-cov"; exit 1; }
	$(CARGO) llvm-cov --workspace --html --output-dir target/coverage
	$(CARGO) llvm-cov --workspace --json --summary-only --output-path target/coverage/summary.json

mem-profile:
	$(CARGO) test --workspace --features dhat,stress --tests 'stress_*' -- --ignored --nocapture

# Reclaim Cargo build artifacts on mununu's siblings under ~/git_repo/.
# Default: dry-run (shows what would be removed). Set SWEEP_APPLY=1 to delete.
# Override age threshold with SWEEP_DAYS=N (default 14).
sweep:
	@./scripts/sweep_targets.sh

lint:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

# Use `cargo run` so the binary path resolves correctly whether or not
# CARGO_TARGET_DIR is set (the dev Docker image redirects target/ to
# /cargo-target so host and container caches stay independent).
verify:
	$(CARGO) run --release --quiet -p mununu-cli -- context summarize $(VERIFY_FILE)

# The gate: `cargo fmt --check` + `cargo clippy -D warnings` + `cargo test --workspace` — the same line
# locally, in the hooks' FULL mode and in ci.yml.
#
# History. The mununu#553 follow-up (2026-09-16) added a `test-api` target here because CI ran a
# separate `cargo test --lib api --features api` beside `make ci`, so the api-gated tests were
# invisible to every local gate (a pre-commit hook saw 2,648 tests where CI saw 3,115). Measured
# 2026-10-26 (run 40380364104): `test-api` was a DUPLICATE — the workspace build already unifies
# mununu-core's `api` + `ast-extract` features through mununu-cli's dependency edge (resolver 3), so
# the 119 `api::` tests run inside `cargo test --workspace` / nextest; `test-api` recompiled core
# with a different feature hash to re-run 71 of them (2.6 min), and the separate CI step ran the ROOT
# package's lib, which has no tests (2.8 min, "0 passed"). The hook gap #553 described was the
# per-crate `cargo test -p mununu-core` building core WITHOUT the api feature — closed in
# scripts/pre-commit (`run_pkg_tests` asks for the unified set), not by a second compile here.
ci: lint test

# Advisory documentation cadence check. Reports whether more than
# THRESHOLD commits (default 10) have landed since the last touch of
# docs/, wiki/, README.md, or examples/. When triggered, lists the
# code areas whose recent commits warrant a wiki / docs review and
# recommends invoking the /docs-traceability skill. See CLAUDE.md
# §Documentation Cadence Guideline for the contributor protocol.
#
# Usage:
#   make docs-audit                       # default threshold = 10
#   make docs-audit DOCS_THRESHOLD=20     # custom threshold
#
# This target does NOT block commits/pushes; exit code 1 on threshold
# breach is advisory only.
verify-repro:
	@scripts/verify-repro.sh

docs-audit:
	@scripts/docs-audit.sh $(DOCS_THRESHOLD)

# The exact engine's bit-cap frontier — the known points, asserted against the release binary
# under the shipped defaults (every MUNUNU_* scrubbed). Not in `ci`: it runs the engine on a
# 32-bit multiplier (~30 s) and a 48-bit one through the cube ladder. `scripts/bitcap_frontier.py
# sweep` is the full measurement; the recorded table (EXP-0031) is kept with the private evaluation data.
frontier-smoke: build
	@scripts/bitcap_frontier.py smoke

clean:
	$(CARGO) clean

# ─────────────────────────────────────────────────────────────────────────
# Experiment / replay verbs (see notebook/0000-overview.md).
# ─────────────────────────────────────────────────────────────────────────

experiment:
	@if [ -z "$(EXP)" ]; then echo "usage: make experiment EXP=NNNN-<slug>  (e.g. EXP=0002-iter-rank-soa)"; exit 2; fi
	@nnnn=$$(echo "$(EXP)" | cut -d- -f1); \
	 slug=$$(echo "$(EXP)" | cut -d- -f2-); \
	 scripts/new_experiment.sh "$$nnnn" "$$slug"

bench-baseline:
	$(CARGO) bench --workspace -- --save-baseline $(BASELINE)

bench-compare:
	$(CARGO) bench --workspace -- --baseline $(BASELINE)
	scripts/bench_diff.sh $(BASELINE)

# Usage: make bench-record EXP=EXP-0001-baseline -- --bench mu_calculus_only
bench-record:
	@if [ -z "$(EXP)" ]; then echo "usage: make bench-record EXP=EXP-NNNN-<slug> -- <bench-args>"; exit 2; fi
	scripts/bench_record.sh $(EXP) $(filter-out $@,$(MAKECMDGOALS))

replay:
	@if [ -z "$(EXP)" ]; then echo "usage: make replay EXP=EXP-NNNN-<slug>"; exit 2; fi
	scripts/repro.sh $(EXP)

publish-prep:
	scripts/check_repro.sh

# `make bench-record EXP=... -- --bench foo` hands its extra words to the script through
# MAKECMDGOALS, so each one must be an acceptable goal. This catch-all swallows them, and
# ONLY when bench-record is among the goals: unscoped it made every typo'd target exit 0
# (roadmap C2; `make definitely-not-a-target` exited 0 and printed `:`).
ifneq ($(filter bench-record,$(MAKECMDGOALS)),)
%:
	@:
endif

# Roadmap C4 — the tracker and main must agree: an open issue that a commit on main or a merged
# PR claims to close is a disagreement. The control runs first (C3): a known closed issue must
# be reported, or the audit is vacuous and the target fails before the live check.
tracker-audit:
	scripts/tracker_audit.sh --control
	scripts/tracker_audit.sh
