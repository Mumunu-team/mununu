# CI, the pre-commit hook, and the image-gated e2e set

> Status: policy (binding). `CLAUDE.md` carries the rules in short form; this file is the full text with the incidents that produced them.

## The gate

> Source of truth: [`scripts/setup-hooks.sh`](../../scripts/setup-hooks.sh) (the hook) and the `ci` / `e2e` / `verify-repro` targets of [`Makefile`](../../Makefile) — surface: CLI-only — contributor instruments run through `make` and the git hook, not a shipped verb

`make ci` = `cargo fmt --check` + `cargo clippy -D warnings` + `cargo test --workspace`. The same command runs locally, in the pre-commit hook (`./scripts/setup-hooks.sh`) and in GitHub Actions; a CI failure a contributor cannot reproduce with `make ci` means the contract is broken. `security-audit` runs `cargo audit`; `dependency-check` is non-blocking. CI-exact reproduction inside the pinned container: [`docs/dev-container.md`](../dev-container.md); finer cargo recipes: [`docs/build-recipes.md`](../build-recipes.md).

## The SVA e2e set and the `mununu-sva` image

> Source of truth: [`scripts/e2e-sweep.sh`](../../scripts/e2e-sweep.sh) · [`docker/Dockerfile.sva`](../../docker/Dockerfile.sva) — surface: CLI-only — a test sweep run in the image, not a shipped verb

The SVA front-end (`sv extract-sva`, `sv verify-auto`) shells out to slang, sv2v and yosys. They are not bundled, so the slang-gated `e2e_` tests are `#[ignore]`d and do not run in `make ci`; their mechanisms are covered by non-ignored unit tests. Run them in the `mununu-sva` image, which extends `mununu-dev` with one tag-pinned public oss-cad-suite release (yosys + yosys-slang, slang, Verilator, btormc, pono, cvc5) plus sv2v — the same tag rosf and monono pin:

```bash
docker build -f docker/Dockerfile.dev -t mununu-dev .     # once (or on toolchain bump)
docker build -f docker/Dockerfile.sva -t mununu-sva .     # once
docker volume create mununu-target                        # once (warm cargo cache)
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features e2e_ -- --ignored --nocapture
make e2e                                                  # the whole sweep, one test per process
```

**One test per process (`make e2e`).** A test that aborts rather than fails — a stack overflow, a panic while unwinding an exhausted BDD arena — does not unwind, so in one shared libtest process it kills the run: no summary, no failure list, every later test silently skipped. That is how the ignored set drifted to 19 unnoticed failures (mununu#503, found while triaging #504). Isolated, an abort is one `CRASH` row and the sweep completes.

**History.** The nightly `.github/workflows/e2e.yml` produced no signal from its addition (2026-07-06) until 2026-09-18: every scheduled run failed at "Build mununu-sva image" because the Dockerfile did `COPY --from=hw-verif:latest`, an image built once from the sibling `hw-verification-uba` repo and never published. The same stale sibling is why the image ran yosys 0.60 while rosf/monono ran 0.68. Restored 2026-09-18 with the tag-pinned public release. Until the nightly has a readable history, the local sweep in the image is the e2e signal to act on.

**Host caveat.** slang ships prebuilts only for `linux-x86_64` and `macos-arm64`; on an Intel macOS host the Linux image is the supported way. `MUNUNU_SLANG_PATH` honours a local build.

### Validate any slang / SVA-touching change in the image — never on the bare host (rule, added 2026-07-13)

> Source of truth: [`adapter::slang::locate_slang`](../../crates/mununu-core/src/adapter/slang/mod.rs) — surface: CLI+API

The dev host commonly has sv2v + yosys but not slang. With slang absent, `sv verify-auto` / `sv extract-sva` cannot run, so a "green" host run has skipped the real path — and the sv2v-only lift is worse than useless for SVA: sv2v 0.0.13 silently drops `assert property` (concurrent and immediate), so the BTOR2 has 0 `bad` nodes and the plain `sv verify` verb reports a vacuous `holds`. (2026-07-13: an sv-yosys safety-cube wiring looked like a spurious `holds`; the whole chain was the sv2v drop, invisible without slang; root-caused with `MUNUNU_KEEP_YOSYS_TMP=1` — `preprocessed.sv` had 0 asserts.) A bare-host `holds` on SVA is presumed vacuous until reproduced under `mununu-sva`.

## Pre-push workspace check (added 2026-06-08)

> Concept: a process rule about which cargo invocations compile which targets.

When a commit touches a field of a struct with multiple construction sites (`OriginalTransition`, `TransitionSpec`, `SignalAnnotation`, `CegarOptions`, `PredicateCubeLiftOptions`, `TransitionDecl`, `ExactSymbolicOptions`, …), run before `git push`:

```bash
cargo check --workspace --all-features --tests
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo fmt --check
cargo test --workspace --all-features --doc    # doctests have their own compilation phase
```

Per-crate `cargo check` does not compile the legacy root bin (`src/main.rs`, `--features cli`), crates that reach the type through workspace edges, test crates that construct it in fixtures, or doctests. Three incidents: `3923822` (K.2b, api-feature typo, fix `04beae6`); `cfee81d` (K.1b-unrolled, the root bin's `src/main.rs:3738` missed, fix `04f01f9`); the same commit's doctests in `abstraction/unrolling.rs`, which ran three commits red because the check did not include doctests (fix `4a07df7`). The full check is heavy — never run it beside a hook.

## Pre-commit hook serialisation (revised 2026-06-03)

> Concept: a scheduling rule for the host; no code artifact.

While a pre-commit hook runs, never launch a second **heavy** cargo workload: `cargo test --workspace` / `make ci` / `cargo build|check|clippy --workspace`, a second `git commit` whose hook would fire, anything that compiles or tests the whole workspace. **Lightweight** work is fine beside a hook's test phase: `cargo check|clippy -p <one crate>`, `cargo test -p <crate> --lib <one test>`, `cargo fmt`, file edits (the hook reads the committed state), plan-doc edits, git queries, `gh run view`.

Why: macOS schedules background hooks at lowered priority (`STAT=SN`); two heavy workloads competing for the compilation pool reproduced the 2026-05-27 R.2.5 incident where 36 / 1,853 tests took 1 h 32 min wall-clock. Test execution is sub-millisecond per area; the wall-clock cost is scheduling. The same rule applies to a timing sweep: a 12-thread test binary beside a 3-shard LIVE sweep stretched one module from 41 s to 2,286 s and produced three spurious timeouts (2026-10-12).

Do not chain commits — land commit N's hook (new HEAD in `git log`) before launching N+1. `--no-verify` needs explicit per-commit user authorisation. Subagents inherit the heavy/lightweight distinction.
