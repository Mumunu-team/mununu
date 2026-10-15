# Environment variables

Every `MUNUNU_*` variable the code reads, one line each, grouped by the subsystem that reads it. The
measurement essays that used to sit beside the exact-engine knobs in `CLAUDE.md` live in the design
notes and consumer briefings linked from each row. A variable is process-global: on the API server it
applies to every request; the per-request CLI flag / API field is named where one exists.

The table is checked against the code by:

```bash
grep -rhoE 'env::var(_os)?\("MUNUNU_[A-Z0-9_]+"' crates/ | grep -oE 'MUNUNU_[A-Z0-9_]+' | sort -u \
  | while read v; do grep -q "\`$v\`" docs/env-vars.md || echo "UNDOCUMENTED: $v"; done
```

## General

> Source of truth: [`crates/mununu-cli/src/main.rs`](../crates/mununu-cli/src/main.rs) — surface: CLI+API

| variable | purpose |
|---|---|
| `RUST_LOG=mununu=info` | logging (`debug` prints the engine's per-stage lines: lift, bit-blast, fixpoint, squaring steps, portfolio members). |
| `MUNUNU_MAX_PROCESS_MEMORY_BYTES` | self-imposed RSS ceiling polled between properties and at each ⊥ re-plan step; past it the remaining properties abstain with a `memory-budget-exceeded` note instead of an allocator `abort()`. Unset ⇒ **auto**: 80 % of a detected cgroup limit, else disabled; `0` disables explicitly. Coarse: an allocation between checkpoints is not caught (mununu#490, #504 C6). |
| `MUNUNU_SELF_EXE` | the binary the reach portfolio re-executes for the isolated `btor2 spacer-check` child (a solver segfault becomes a child exit read as ABSTAIN). Set only when the running executable cannot be located. |
| `MUNUNU_UPDATE_API_SCHEMAS` (`=1`) | with `cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1`, rewrites `docs/api-schemas/*.schema.json` from the Rust types instead of failing on drift. |
| `MUNUNU_R_S5_DUMP_PATH` | debug aid in the CLI loader: dumps the post-widening JSON of a sidecar load for inspection. |

## Subprocess tools (locators and keep-temp switches)

> Source of truth: [`adapter::yosys::locate_yosys`](../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: CLI+API

| variable | purpose |
|---|---|
| `MUNUNU_YOSYS_PATH`, `MUNUNU_SV2V_PATH`, `MUNUNU_SLANG_PATH`, `MUNUNU_VERILATOR_PATH`, `MUNUNU_PONO_PATH`, `MUNUNU_CVC5_PATH`, `MUNUNU_MATHSAT_PATH` | explicit binary paths, checked before `$PATH`. None of these tools is bundled; the `mununu-sva` image pins them (`docker/README.md`). |
| `MUNUNU_YOSYS_SLANG_PLUGIN` | path of the yosys-slang plugin (`read_slang`) used for the RTL lift of designs `read_verilog` rejects; SVA is extracted separately by slang. |
| `MUNUNU_YOSYS_FRONTEND` | prefer `slang` over `read_verilog` + sv2v for the lift (the CLI's `--frontend` is the per-request form). |
| `MUNUNU_YOSYS_SETUNDEF_ANYSEQ` (`=1`), `MUNUNU_YOSYS_SETUNDEF_ANYCONST` (`=1`) | the lift's init policy for undefined bits: one free input per cycle (`anyseq`), or one nondeterministic constant per bit (`anyconst`); `ANYSEQ` wins if both are set; default `-zero` (which can mask bugs). |
| `MUNUNU_KEEP_YOSYS_TMP` (`=1`), `MUNUNU_KEEP_VERILATOR_TMP` (`=1`) | keep the tool's temp directory (how the sv2v-drops-asserts trap was root-caused: `preprocessed.sv` had 0 asserts). |

## Exact-symbolic engine (OxiDD ROBDD)

> Source of truth: [`adapter::btor2::symbolic_bitblast`](../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI+API

Design notes: [`docs/design/bdd-variable-ordering.md`](design/bdd-variable-ordering.md); briefings `2026-10-exact-reachable-restriction.md`, `2026-10-lever-gating-ladder.md`, `2026-10-lever-plan-open-items.md`, `2026-10-exact-inputs-in-the-state.md` under `consumer-briefings/`.

| variable | purpose |
|---|---|
| `MUNUNU_BDD_MAX_BITS` | **bit cap**: register+input bits in the property cone before the engine abstains (`abstained on the BIT CAP`). The exact engine admits cones up to 40 bits by default and raises its own cap to fit the cone up to 192; above that it abstains with a reason unless told otherwise. The width it can actually decide depends on the shape — see the frontier table. Unset, the cap is `max(40, min(cone, 192))` (`default_cap_for_cone`); the recoverability verb and the ⊥ re-plan rescues retry a refused cone once at the 1024-bit ceiling (the cap ladder). The deciding width per shape is measured in [`verifying-rtl.md`](verifying-rtl.md#what-decides-at-what-width--the-measured-frontier). |
| `MUNUNU_BDD_ARENA_NODES` | OxiDD arena size; scales the bit-blast node budget and the fixpoint budget. Defaults `1<<24` (≤ 40-bit cones) / `1<<26` (wider), since deciding consumer cones measured 92.5 % occupancy of the old arena; exhaustion inside one apply is an uncatchable abort (mununu#553). |
| `MUNUNU_BDD_ITER_BUDGET` | fixpoint iteration budget (`abstained on the ITERATION budget`), default `1<<20` — the deterministic total-work bound. Raster-shaped cones follow `iters ≈ 1.95·h·v` (720p ≈ 230 %, 1080p ≈ 460 % of it); the squaring rescue below is what decides those. |
| `MUNUNU_BDD_SQUARING` | the **squaring rescue**, on by default (`=0` disables): a pure-reachability fixpoint (`EF`/`AG` shape) still iterating after 4,096 steps is read off the transitive closure of the transition relation, built in its own arena by iterative squaring (`max(48, state bits + 1)` squarings); and a fair-EG ν (`verify-justice`'s `E_C G true`) past the threshold is read off the same closure as `EF(Core)`, `Core(s) = ⋀ₖ ∃t. R⁺(s,t) ∧ R⁺(t,s) ∧ lₖ(t)` (Touati–Brayton–Kurshan). A closure that does not compress is refused at 80 % of the arena and the iteration resumes. For an all-1-bit-state design the closure's variables follow the circuit visit rank (an AIG's latch order is not significance-monotone). |
| `MUNUNU_BDD_FIXPOINT_NODES` | fixpoint live-node budget (`abstained on the fixpoint NODE budget`), default 80 % of the arena; `0` = no explicit cap. A resource guard, not a decidability predictor. |
| `MUNUNU_BDD_FIXPOINT_ITERS` | the iteration half of the LATENCY bound (large *and* still growing); `0` disables that bound (the arena-safety net still applies). |
| `MUNUNU_BDD_FIXPOINT_REUSE` (`=0`) | disable the restart rule (a nested fixpoint of the same polarity restarts from its previous value); legacy evaluation. |
| `MUNUNU_BDD_TIME_BUDGET_MS` | opt-in wall-clock budget for the fixpoint. **Default `0` = off**: a clock makes the VERDICT host-dependent, and that ⊥ is indistinguishable from a real one. The ladder's per-attempt deadlines are separate and bounded. |
| `MUNUNU_EXACT_LADDER_MS` | the first stage's slice of the exact **ladder** (default 2000 ms): unrestricted → on a retryable abstention the reachable-state restriction with a 5 s probe → unrestricted remainder. Runs in `btor2 verify-justice`, `sv verify-auto`'s exact member and the ⊥ re-plan rescues. |
| `MUNUNU_BDD_REACH_ARENA`, `MUNUNU_BDD_REACH_BUDGET_MS` | the restriction's Squarer arena (`1<<25`) and the forward-reach phase's clock outside the ladder (30 s, ≤ ¼ of the model deadline); past it the restriction is declined and evaluation proceeds unrestricted — it changes which path runs, never a verdict. |
| `MUNUNU_BDD_VAR_ORDER` | `interleaved` (default) / `cell-major` / `circuit` (opt-in: interleaved keyed by a DFS from the property roots; measured as a per-shape trade on HWMCC LIVE). Never changes a verdict. |
| `MUNUNU_COI` | cone-of-influence granularity: default bit-level (a constant part-select keeps the slice); `=signal` restores whole-register cones for a differential run (mununu#602, #642). |
| `MUNUNU_BDD_THREADS` | OxiDD worker threads (default 1 — measured as a net loss under the portfolio); the allocation counters read up to `t × 64 Ki` nodes low under `t` workers. |
| `MUNUNU_BDD_CACHE_DIV` | apply-cache size as `arena / div` (default 4; `64` saves 60–80 % on shallow properties and costs +291 % on the representation-bound relational class — a knob, no automatic policy survived measurement). |
| `MUNUNU_NO_ANTECEDENT_PROPAGATE` | disable antecedent constant propagation (`sig == K |-> C` has `K` propagated through `C`'s cone); per-request `--no-antecedent-propagate` / `no_antecedent_propagate` (mununu#629). |
| `MUNUNU_NO_INPUT_STATE` | disable **inputs in the state** (a property reading a primary input is decided over the product `(s, i)`); the refusal returns. Per-request `--no-input-state` / `no_input_state` (mununu#656). |
| `MUNUNU_NO_ANTECEDENT_SHADOW` | read by nothing since mununu#677 (the shadow synthesis was removed); accepted for compatibility. |
| `MUNUNU_BDD_REPORT_PEAK`, `MUNUNU_BDD_REPORT_WORK` | one line per evaluation with the peak node count (after a collection) or the host-independent BDD-op counter (no collection — the one the profiling scripts set). A peak at or near the arena is a lower bound. |
| `MUNUNU_BDD_STATS`, `MUNUNU_BDD_ORDER_DEBUG`, `MUNUNU_REPORT_BDD_LEVELS`, `MUNUNU_VALIDATE_BDD_LEVELS` | diagnostics: OxiDD apply-cache statistics (needs `--features oxidd-statistics`); the variable order as built with the `order_verdict` reading; the bit-blaster's level report; a level-consistency check. None changes a verdict. |

## Predicate-cube engines (`explicit`, `symbolic`) and the ⊥ re-plan

> Source of truth: [`adapter::btor2::kmts_lift::MustEdgeInference`](../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L807) (`--must-edge-inference` / `must_edge_inference`) — surface: CLI+API

| variable | purpose |
|---|---|
| `MUNUNU_CUBE_SMT_RLIMIT` | a deterministic Z3 resource limit on every cube SMT query (may / must edges); past it the answer is read as the conservative side (`Unknown → NotMust`, `Unknown → May`) — more ⊥, never a wrong verdict. The way to stop a bare `explicit` run grinding on a wide combinational cone. |
| `MUNUNU_CUBE_POSTIMAGE_BACKEND` | `bdd` (default) / `smt` / `check` for the post-image may-relation; `check` runs both and logs every differing cell — the differential-oracle mode. |
| `MUNUNU_CUBE_POSTIMAGE_THREADS` | workers on the `smt` post-image path only. |
| `MUNUNU_CRAIG_REFINE` (`=1`) | CEGAR predicate source = Craig interpolants (default: weakest precondition) for the non-planner callers. |
| `MUNUNU_NO_INTERP_DISCOVERY` (`=1`) | skip interpolant-based relational predicate discovery in the recoverability rescue. |
| `MUNUNU_ASSUMPTION_BUDGET_MS` | wall budget of `--discover-assumptions` (narrow-input partition search). |
| `MUNUNU_BTOR2_SMART_INIT_DEFAULTS` | opt-in init defaults for `state` cells without `init` in the bit-blaster (the mununu#505 / #579 reading is free-at-cycle-0). |

## `sv verify-auto` run diagnostics

> Source of truth: [`adapter::slang::verify_auto`](../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API

| variable | purpose |
|---|---|
| `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` | path of a SIGKILL-survivable NDJSON breadcrumb, one record per property, flushed after every write. **Not authoritative for a portfolio run**: the planner's merge writes nothing, so a `main`-phase `unknown` can end the run decided. Use it for what was decided at the moment of an external kill (mununu#504 C7). |
| `MUNUNU_SHADOW_BTOR2_DUMP` | path for the BTOR2 the engine actually runs (lift, `$past` shadows, pins, mutations, the formulas as comments). **Excludes** the inputs-in-the-state lift, which runs per property inside the engine (mununu#552). |
| `MUNUNU_PROPERTY_TIMING` (`=1`) | per-property wall time in the report — "which property is eating the run?" (mununu#504). |
| `MUNUNU_SPCR_OUT_DIR` | emit the array-free design the SPCR pass produced, for a manual marginal-reach check. |
| `MUNUNU_INTERP_DUMP` | path for the failing cvc5 interpolation query of the native interpolation engine — distinguishes an interpolation incompleteness from a serialisation bug. |
| `MUNUNU_OWNED_TIMEOUT_MS` | per-check timeout of the owned (in-process) reachability members. |
| `MUNUNU_BOOLECTOR_TRACE` (`=1`) | per-bound `sat()` wall times of the native Boolector BMC member. |

## Test-harness knobs (read only by `#[ignore]`d tests and probes)

> Source of truth: [`crates/mununu-core/tests/differential_oracle_e2e.rs`](../crates/mununu-core/tests/differential_oracle_e2e.rs) — surface: CLI-only — read by ignored tests run through `cargo test`, never by a shipped binary

| variable | purpose |
|---|---|
| `MUNUNU_HWMCC_DIR`, `MUNUNU_HWMCC_FLAT`, `MUNUNU_HWMCC_GT`, `MUNUNU_HWMCC_MAXBYTES`, `MUNUNU_HWMCC_LIVE_DIR` | user-downloaded HWMCC corpora (bv track, flat BTOR2, ground truth `basename safe\|unsafe`, size cap, the LIVE set for `scripts/hwmcc_live_study.sh`). |
| `MUNUNU_DISCOVER_DIR`, `MUNUNU_DISCOVER_MAXBYTES`, `MUNUNU_SPCR_PROBE_DIR`, `MUNUNU_INTERP_PROBE` | external BTOR2 directories / files for the predicate-discovery, SPCR and interpolation sweeps. |
| `MUNUNU_NATIVE_MAXK`, `MUNUNU_NATIVE_MS`, `MUNUNU_CENSUS_EXPLICIT` (`=1`), `MUNUNU_PARITY_ONLY` | the census / adjudication tests' bound, per-check budget, opt-in explicit-engine fallback, and name filter. |
| `MUNUNU_PROBE_BTOR`, `MUNUNU_PROBE_BTOR2`, `MUNUNU_PROBE_ATOMS`, `MUNUNU_PROBE_GOOD`, `MUNUNU_PROBE_OP`, `MUNUNU_PROBE_DEPTH`, `MUNUNU_PROBE_RASTER`, `MUNUNU_PROBE_SECOND_SHADOW`, `MUNUNU_E1_ARM` | one-process-per-case reproduction probes (stack-depth, raster size, apply-call arms); exit 0 = survived, 101/134 = reproduced. |
| `MUNUNU_R3_UPDATE_BASELINE` (`=1`) | rewrite the R3 Kleene baseline fixture instead of comparing against it. |
