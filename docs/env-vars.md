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

and, both ways, by `crates/mununu-core/tests/policy/env_vars_documented_d1.rs`: every `MUNUNU_*` name the
crates read has a row here, and every row names a variable something reads (a row may say
`scripts/…` when only a script reads it). `0` means **off** for every budget knob.

## General

> Source of truth: [`crates/mununu-cli/src/main.rs`](../crates/mununu-cli/src/main.rs) — surface: CLI+API

| variable | purpose |
|---|---|
| `MUNUNU_BUILD_SHA` | **build time only** (read by `crates/mununu-cli/build.rs`, never at run time): the commit to record as the binary's identity, for builds without `.git` (the production Dockerfile's build arg). Unset: the build script asks git; neither answering means no identity (mununu#715). |
| `MUNUNU_BUILD_DIRTY` | **build time only**: `0` (tracked files matched the commit) or `1`, paired with `MUNUNU_BUILD_SHA`. Without it a given sha is not recorded — the dirty bit is never guessed. |
| `RUST_LOG=mununu=info` | logging (`debug` prints the engine's per-stage lines: lift, bit-blast, fixpoint, squaring steps, portfolio members). |
| `MUNUNU_MAX_PROCESS_MEMORY_BYTES` | self-imposed RSS ceiling polled between properties and at each ⊥ re-plan step; past it the remaining properties abstain with a `memory-budget-exceeded` note instead of an allocator `abort()`. Unset ⇒ **auto**: 80 % of a detected cgroup limit, else disabled; `0` disables explicitly. Coarse: an allocation between checkpoints is not caught (mununu#490, #504 C6). |
| `MUNUNU_DEPLOYMENT_CONFIG` | the deployment configuration (`mununu.toml`: template packs, `enabled_domains`, `[licensing] file`, `[server] cors_origins` / `file_root`) when neither `--deployment-config` nor `server --config` names one; an unreadable file is an error, not a fallback to the defaults ([`deployment`](../crates/mununu-core/src/deployment.rs)). |
| `MUNUNU_LICENSE_FILE` | the signed licence file (`mununu.lic`) when `[licensing] file` in `mununu.toml` does not name one; after both, `/etc/mununu/mununu.lic`. Read by `mununu licence status`, `GET /api/v1/licence`, the `licence` field of `/api/v1/health` and of every `sv verify-auto` report; never by an engine ([`licence`](../crates/mununu-core/src/licence.rs), [`docs/licensing.md`](licensing.md)). |
| `MUNUNU_USAGE_LEDGER` | the opt-in local usage ledger (NDJSON, one record per `sv verify-auto` run on either surface: time, verb, surface, version, build, licence state / tier / team, result, verdict counts, wall — **no design content**) when `[licensing] ledger` in `mununu.toml` does not name one. Unset = off. A failed write is a stderr `warning:` (CLI) / server-log `warn` (API) and never fails the run. Shown as `ledger` by `mununu licence status` and `GET /api/v1/licence` (Track H3, `docs/licensing.md`). |
| `MUNUNU_UPDATE_API_SCHEMAS` (`=1`) | with `cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1`, rewrites `docs/api-schemas/*.schema.json` from the Rust types instead of failing on drift. |
| `MUNUNU_REQUIRE_TOOLS` (`=1`) | test-only (mununu#981): a tool-gated test whose tool is absent FAILS instead of skipping (`adapter::require_tool` — the yosys/`read_slang` probes and the refine cvc5 test). Set by the nightly's in-image unit pass, where every tool is supposed to be present; unset, an absent tool skips as before. |
| `MUNUNU_R_S5_DUMP_PATH` | debug aid in the CLI loader: dumps the post-widening JSON of a sidecar load for inspection. |
| `MUNUNU_VERSION_LINE` | **build-time**, not runtime: `build.rs` sets it and `clap` bakes it into `mununu --version` (`0.5.10 (24bb5d7)`, `-dirty` when the tree was); nothing reads it at run time (mununu#715). |

## Subprocess tools (locators and keep-temp switches)

> Source of truth: [`adapter::yosys::locate_yosys`](../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: CLI+API

| variable | purpose |
|---|---|
| `MUNUNU_YOSYS_PATH`, `MUNUNU_SLANG_PATH`, `MUNUNU_VERILATOR_PATH`, `MUNUNU_BTORMC_PATH`, `MUNUNU_PONO_PATH`, `MUNUNU_RIC3_PATH`, `MUNUNU_CVC5_PATH`, `MUNUNU_MATHSAT_PATH` | explicit binary paths, checked before `$PATH`. None of these tools is bundled; the `mununu-sva` image pins them (`docker/README.md`). |
| `MUNUNU_YOSYS_SLANG_PLUGIN` | only when set: yosys loads that yosys-slang plugin (`slang.so`, or a bare name yosys resolves) with `-m` for the RTL lift. Unset = the built-in `read_slang` (yosys ≥ 0.67, the pinned `mununu-sva` image); there is no fixed plugin-path probe. The lift is slang-only (N1, 2026-11-04): a yosys with neither lifts no SystemVerilog, and the error says so. SVA is extracted separately by slang. |
| `MUNUNU_SV2V_PATH` | **dev-only** (`test_support` builds): the `read_verilog` + sv2v differential oracle (`SvFrontend::Verilog`, two lifts of one source compared). Never read by a product build; `MUNUNU_YOSYS_FRONTEND` and `MUNUNU_USE_SV2V` are gone (2026-11-04). |
| `MUNUNU_YOSYS_SETUNDEF_ANYSEQ` (`=1`), `MUNUNU_YOSYS_SETUNDEF_ANYCONST` (`=1`) | the lift's init policy for undefined bits: one free input per cycle (`anyseq`), or one nondeterministic constant per bit (`anyconst`); `ANYSEQ` wins if both are set; default `-zero` (which can mask bugs). |
| `MUNUNU_KEEP_YOSYS_TMP` (`=1`), `MUNUNU_KEEP_VERILATOR_TMP` (`=1`) | keep the tool's temp directory — the staged `work.sv` and the exact yosys script (how the 2026-07 sv2v-drops-asserts trap was root-caused, back when an sv2v pass wrote `preprocessed.sv`; that pass is the dev-only oracle now). |
| `MUNUNU_DEBUG_YOSYS_SCRIPT` (set) | print the yosys working directory and the exact script before each lift run, on stderr (`adapter::yosys`). |
| `MUNUNU_LIFT_CACHE_DIR` | a directory; the SV → BTOR2 lift stores its raw tool outputs there (`design.btor`, `hier.json`, `clocks.json` and the process facts), keyed by everything the tools were told — the staged sources, the exact yosys script, every file under an include dir, the tool banners (and, in a `test_support` build running the oracle, the sv2v argv) — and reuses them on an identical lift; everything mununu computes from those outputs runs on a hit as on a miss (`adapter/yosys/lift_cache.rs`, N0.2, 2026-11-03). Unset = every lift runs. `make e2e`, `e2e-oneshot` and `e2e-parallel` set it under the cargo target dir, so the image's volume keeps it between sweeps; since format 3 (mununu#956) the key also covers absolutely-`include`d files, include contents byte for byte, and the yosys binary's and slang plugin's size and mtime. A bypass (an unusable dir, an include tree over the walk cap, an `include` the key cannot resolve — a macro argument or a `..` path — or a plugin it cannot find) is logged at `warn` and the lift runs. |

## Exact-symbolic engine (OxiDD ROBDD)

> Source of truth: [`adapter::btor2::symbolic_bitblast`](../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI+API

Design notes: [`docs/design/bdd-variable-ordering.md`](design/bdd-variable-ordering.md); briefings `2026-10-exact-reachable-restriction.md`, `2026-10-lever-gating-ladder.md`, `2026-10-lever-plan-open-items.md`, `2026-10-exact-inputs-in-the-state.md` under `consumer-briefings/`.

| variable | purpose |
|---|---|
| `MUNUNU_BDD_MAX_BITS` | **bit cap**: register+input bits in the property cone before the engine abstains (`abstained on the BIT CAP`). The exact engine admits cones up to 40 bits by default and raises its own cap to fit the cone up to 192; above that it abstains with a reason unless told otherwise. The width it can actually decide depends on the shape — see the frontier table. Unset, the cap is `max(40, min(cone, 192))` (`default_cap_for_cone`); the recoverability verb and the ⊥ re-plan rescues retry a refused cone once at the 1024-bit ceiling (the cap ladder). The deciding width per shape is measured in [`verifying-rtl.md`](verifying-rtl.md#what-decides-at-what-width--the-measured-frontier). |
| `MUNUNU_BDD_ARENA_NODES` | OxiDD arena size; scales the bit-blast node budget and the fixpoint budget. Defaults `1<<24` (≤ 40-bit cones) / `1<<26` (wider), since deciding consumer cones measured 92.5 % occupancy of the old arena; exhaustion inside one apply is an uncatchable abort (mununu#553). |
| `MUNUNU_BDD_ITER_BUDGET` | fixpoint iteration budget (`abstained on the ITERATION budget`), default `1<<20` — the deterministic total-work bound; `0` = off (since B-1; it used to mean zero iterations, i.e. abstain at once). Raster-shaped cones follow `iters ≈ 1.95·h·v` (720p ≈ 230 %, 1080p ≈ 460 % of it); the squaring rescue below is what decides those. |
| `MUNUNU_BDD_SQUARING` | the **squaring rescue**, on by default (`=0` disables): a pure-reachability fixpoint (`EF`/`AG` shape) still iterating after 4,096 steps is read off the transitive closure of the transition relation, built in its own arena by iterative squaring (`max(48, state bits + 1)` squarings); and a fair-EG ν (`verify-liveness --justice`'s `E_C G true`) past the threshold is read off the same closure as `EF(Core)`, `Core(s) = ⋀ₖ ∃t. R⁺(s,t) ∧ R⁺(t,s) ∧ lₖ(t)` (Touati–Brayton–Kurshan). A closure that does not compress is refused at 80 % of the arena and the iteration resumes. For an all-1-bit-state design the closure's variables follow the circuit visit rank (an AIG's latch order is not significance-monotone). |
| `MUNUNU_BDD_FIXPOINT_NODES` | fixpoint live-node budget (`abstained on the fixpoint NODE budget`), default 80 % of the arena; `0` = off (no cap). A resource guard, not a decidability predictor. |
| `MUNUNU_BDD_FIXPOINT_ITERS` | the iteration half of the LATENCY bound (large *and* still growing); `0` = off for that bound (the arena-safety net still applies). |
| `MUNUNU_BDD_FIXPOINT_REUSE` (`=0`) | disable the restart rule (a nested fixpoint of the same polarity restarts from its previous value); legacy evaluation. |
| `MUNUNU_BDD_TIME_BUDGET_MS` | opt-in wall-clock budget for the fixpoint. **Default `0` = off**: a clock makes the VERDICT host-dependent, and that ⊥ is indistinguishable from a real one. The ladder's per-attempt deadlines are separate and bounded. |
| `MUNUNU_EXACT_LADDER_MS` | the first stage's slice of the exact **ladder** (default 2000 ms): unrestricted → on a retryable abstention the reachable-state restriction with a 5 s probe → unrestricted remainder. Runs in `btor2 verify-liveness --justice`, `sv verify-auto`'s exact member and the ⊥ re-plan rescues. |
| `MUNUNU_BDD_REACH_ARENA`, `MUNUNU_BDD_REACH_BUDGET_MS` | the restriction's Squarer arena (`1<<25`) and the forward-reach phase's clock outside the ladder (30 s, ≤ ¼ of the model deadline); past it the restriction is declined and evaluation proceeds unrestricted — it changes which path runs, never a verdict. |
| `MUNUNU_BDD_VAR_ORDER` | `interleaved` (default) / `cell-major` / `circuit` (opt-in: interleaved keyed by a DFS from the property roots; measured as a per-shape trade on HWMCC LIVE). Never changes a verdict. |
| `MUNUNU_COI` | cone-of-influence granularity: default bit-level (a constant part-select keeps the slice); `=signal` restores whole-register cones for a differential run (mununu#602, #642). |
| `MUNUNU_BDD_THREADS` | OxiDD worker threads (default 1 — measured as a net loss under the portfolio); the allocation counters read up to `t × 64 Ki` nodes low under `t` workers. |
| `MUNUNU_BDD_CACHE_DIV` | apply-cache size as `arena / div` (default 4; `64` saves 60–80 % on shallow properties and costs +291 % on the representation-bound relational class — a knob, no automatic policy survived measurement). |
| `MUNUNU_NO_ANTECEDENT_PROPAGATE` | disable antecedent constant propagation (`sig == K |-> C` has `K` propagated through `C`'s cone); per-request `--no-antecedent-propagate` / API `antecedent_propagate: false` (mununu#629). |
| `MUNUNU_NO_INPUT_STATE` | disable **inputs in the state** (a property reading a primary input is decided over the product `(s, i)`); the refusal returns. Per-request `--no-input-state` / API `input_state: false` (mununu#656). |
| `MUNUNU_BDD_REPORT_PEAK`, `MUNUNU_BDD_REPORT_WORK` | one line per evaluation with the peak node count (after a collection) or the host-independent BDD-op counter (no collection — the one the profiling scripts set). A peak at or near the arena is a lower bound. |
| `MUNUNU_BDD_STATS`, `MUNUNU_BDD_ORDER_DEBUG`, `MUNUNU_REPORT_BDD_LEVELS`, `MUNUNU_VALIDATE_BDD_LEVELS` | diagnostics: OxiDD apply-cache statistics (needs `--features oxidd-statistics`); the variable order as built with the `order_verdict` reading; the bit-blaster's level report; a level-consistency check. None changes a verdict. |
| `MUNUNU_REPORT_BDD_ITERS` | one stderr line per exact-engine fixpoint step (`[mununu-bdd] step=… total=… live=… step_ms=…`): the instrument that brackets a step-level hang (mununu#816 — a step is one uninterruptible BDD apply, so the budget polls run only between steps). Off by default. |
| `MUNUNU_ISOLATE_ENGINES` | run the BDD engines in a **child process** so an OxiDD stack overflow or abort is an exit code the parent turns into an abstention, not a dead run (mununu#543 W3, `adapter/engine_isolate.rs`). Off unless set. |
| `MUNUNU_ISOLATE_CHILD` | set by the parent on the child it spawns, so the child does not isolate again; never set by hand. |

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
| `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` | path of a SIGKILL-survivable NDJSON breadcrumb, one record per property, flushed after every write. Under a portfolio every member appends its own `main` / `escalated` records (a later member writes `unknown` for a row it left to an earlier one — mununu#733), and the planner appends one `phase: "merged"` record per property after the merge and the ⊥ re-plan: **the last record per property is the verdict that ships**; a `main`-phase `unknown` is not. Use it for what was decided at the moment of an external kill (mununu#504 C7). Since roadmap J3 every record carries `build` (the writing binary's identity) and `sv verify-auto --resume-from FILE` reads the file back: the rows a run of the **same build** decided are carried, not re-run; another build's breadcrumb is refused. |
| `MUNUNU_SHADOW_BTOR2_DUMP` | path for the BTOR2 the engine actually runs (lift, `$past` shadows, pins, mutations, the fault-injection posture's inputs and budget counter with a `; FAULT MODEL:` header line, the formulas as comments). **Excludes** the inputs-in-the-state lift, which runs per property inside the engine (mununu#552). |
| `MUNUNU_RESCUE_MONITOR_DUMP` | a directory; the ⊥ re-plan writes the `bad`-monitor BTOR2 it hands the reach portfolio, one file per rescued row (`<dir>/<property>.monitor.btor2`, the `EF` dual's monitor included), so an external engine (rIC3, pono, SymbiYosys) can be probed on exactly what the portfolio saw. Unset = no files. |
| `MUNUNU_LIFT_TIMEOUT_MS` | wall-clock cap on each lift subprocess — slang (extraction) and yosys — default 15 min (`900000`); `0` disables (mununu#504). A slang timeout is a hard error (no property list exists yet); a yosys timeout degrades the run. A full OpenTitan IP lifts in seconds (edn: 7 s), so the default is rarely the limit. |
| `MUNUNU_PROPERTY_BUDGET_MS` | per-property wall-clock budget across the engine ladder, default 15 min; `0` disables. The exact engine polls it every fixpoint iteration, squaring step and reach-probe step (mununu#804, 2026-10-26: before that only the opt-in `MUNUNU_BDD_TIME_BUDGET_MS` deadline stopped it, and a 300 s HWMCC budget ran four hours), and — mununu#816, 2026-10-28 — at every bit-blast op and relation-build conjunct, refusing a fixpoint step BEFORE it starts when the time left could not hold even the previous one (a step is one uninterruptible BDD apply, so the polls only run between steps; `vis_arrays_bufferAlloc` ran 27 min under a 300 s budget inside one step); the abstention is `deadline`. An exhausted budget leaves the property `unknown` with a `budget` note, never a verdict. Observed between engine calls, at the cube's CEGAR round boundaries, in its post-image and label loops and as the clamp on every SMT query; the exact engine's own fixpoint clock stays `MUNUNU_BDD_TIME_BUDGET_MS`. A per-property expiry marks ITS row (`bottom_reason: budget-expired`) and the rows after it are still attempted; only the run budget stops the run. Pair it with `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` on an IP-sized run. |
| `MUNUNU_VERIFY_BUDGET_MS` | whole-run wall-clock budget, default 1 h; `0` disables. Once spent, the remaining properties abstain (`unknown`) and the re-plan passes stop; prior verdicts are kept. |
| `MUNUNU_RESCUE_SLICE_MS` | the ⊥ re-plan's wall-clock slice per portfolio-backed step — the reach portfolio on a `bad`-monitor, the fair-cycle l2s rescue, the safety-witness BMC — default 60 s (the wall the portfolio always ran under); `0` disables (the per-property budget alone bounds the step). A child of the per-property budget, so it never extends it. The portfolio's members take their time from it (mununu#726: the native BMC's fixed 5-s per-query timeout let one to three of eight identical rows decide per run on a 200-register IP); a row left ⊥ when the slice ran out carries `bottom_reason: budget-expired`. |
| `MUNUNU_PROPERTY_TIMING` (`=1`) | per-property wall time on stderr (the REPORT carries it regardless: `properties[].wall_ms` on every row since J4/J4c, and the top-level `timing { run_ms, lift_ms, properties_ms, unattributed_ms }` since J4c — this variable only prints the lines) — "which property is eating the run?" (mununu#504): `[mununu-timing] property=<name> outcome=<label> elapsed_ms=<n> phase=main` for the cube pass and `… phase=replan lane=<safety\|liveness\|trap-certificate\|recoverability\|reachability\|none>` for each row the ⊥ re-plan touched (the `phase` / `lane` fields are trailing and additive, 2026-10-18). Since J4 (2026-11-08) the wall is measured regardless and shipped as every property's `wall_ms` in the report; the variable only prints the lines. |
| `MUNUNU_VERIFY_AUTO_ROW_FACTS` | path of an NDJSON file written ONCE at the end of the run (N0.1, 2026-11-03): one record per property — `index`, `property`, `label`, `kind`, `formula`, `outcome`, `decided_by`, `bottom_reason`, `cone_hash` (the FNV-1a of the formula and the property's BTOR2 cone slice, the ledger's "did what decides this row change?" key). The executable ledger (`scripts/ledger_check.py`, `make ledger-check`) reads it; seconds come from `MUNUNU_PROPERTY_TIMING`. A path that cannot be written is one `warn`, never a failure. |
| `MUNUNU_VERIFY_AUTO_ONLY_ROWS` | comma-separated property names or labels: only those rows run, every other row is `unknown` with `bottom_reason: not-attempted` and the ⊥ re-plan leaves it alone; `-` runs no row (the cone hashes of every row for the price of the lift). The ledger's re-run of the rows whose cone changed (N0.1). Unset = every row. |
| `MUNUNU_SPCR_OUT_DIR` | emit the array-free design the SPCR pass produced, for a manual marginal-reach check. |
| `MUNUNU_INTERP_DUMP` | path for the failing cvc5 interpolation query of the native interpolation engine — distinguishes an interpolation incompleteness from a serialisation bug. |
| `MUNUNU_KLIVENESS_MONITOR_DUMP` | path prefix: the k-liveness ladder writes each round-counter monitor it builds to `<prefix>.k<width>.btor2` (justice rung and the response verbs' fallback), so another engine can decide the same monitor, e.g. `btor2 verify --owned-only`. Diagnostic only; never changes a verdict. |
| `MUNUNU_OWNED_TIMEOUT_MS` | per-check timeout of the owned (in-process) reachability members. |
| `MUNUNU_BOOLECTOR_TRACE` (`=1`) | per-bound `sat()` wall times of the native Boolector BMC member. |

## Test-harness knobs (read only by `#[ignore]`d tests and probes)

> Source of truth: [`crates/mununu-core/tests/differential_oracle_e2e.rs`](../crates/mununu-core/tests/differential_oracle_e2e.rs) — surface: CLI-only — read by ignored tests run through `cargo test`, never by a shipped binary

| variable | purpose |
|---|---|
| `MUNUNU_HWMCC_DIR`, `MUNUNU_HWMCC_FLAT`, `MUNUNU_HWMCC_GT`, `MUNUNU_HWMCC_MAXBYTES`, `MUNUNU_HWMCC_LIVE_DIR`, `MUNUNU_HWMCC_SKIP`, `MUNUNU_HWMCC_TIMEOUT_MS`, `MUNUNU_HWMCC_ARRAY_DIR` | user-downloaded HWMCC corpora (bv track, flat BTOR2, ground truth `basename safe\|unsafe`, size cap, the LIVE set for `scripts/hwmcc_live_study.sh`); `MUNUNU_HWMCC_ARRAY_DIR` = the HWMCC'20 BTOR2 `array` track directory the `#[ignore]`d `wall_class_safety_on_memory` matrix case reads (nothing from it is vendored); `MUNUNU_HWMCC_SKIP` = comma-separated name substrings the adjudication harness skips, and `MUNUNU_HWMCC_TIMEOUT_MS` (default 300 000) its per-instance run budget, so one crashing or hanging instance does not void the table (mununu#738). |
| `MUNUNU_E2E_LIFT_DUMP` | path; `e2e_lifted_register_names_carry_no_per_run_temp_path` writes the edn IP's first lift there, to diff a lift by hand (mununu#806). |
| `MUNUNU_DEBUG_DUMP` | path of a `MUNUNU_SHADOW_BTOR2_DUMP` file; the ignored probe `debug_antecedent_check_on_dump` (`verify_auto::fault_posture_debug`) runs the antecedent-vacuity question on it under both flags. |
| `MUNUNU_DISCOVER_DIR`, `MUNUNU_DISCOVER_MAXBYTES`, `MUNUNU_SPCR_PROBE_DIR`, `MUNUNU_INTERP_PROBE` | external BTOR2 directories / files for the predicate-discovery, SPCR and interpolation sweeps. |
| `MUNUNU_NATIVE_QUERY_RLIMIT` | the native BMC / k-induction member's per-check budget in z3 **resource units**, overriding the nominal conversion (mununu#893: the default is the nominal allowance — 5 s, or the raised `--timeout-ms` — × 3 000 units per nominal ms, a deterministic stop that is the same on every host). `0` = no resource limit. A stop on it is a `portfolio.abstained` entry for `native` and a `portfolio-budget-stop` note, never a silent `unknown`. |
| `MUNUNU_NATIVE_QUERY_TIMEOUT_MS` | an opt-in per-check **wall clock** for the native member, off by default: like `MUNUNU_BDD_TIME_BUDGET_MS`, a clock makes the verdict host-dependent — the very flip mununu#893 removed. The run budgets still cap every check's wall when they are in effect; a stop on either wall is named as the wall (`budget-expired`). |
| `MUNUNU_NATIVE_MAXK`, `MUNUNU_NATIVE_MS`, `MUNUNU_CENSUS_EXPLICIT` (`=1`), `MUNUNU_PARITY_ONLY` | the census / adjudication tests' bound, per-check budget, opt-in explicit-engine fallback, and name filter. |
| `MUNUNU_PROBE_BTOR`, `MUNUNU_PROBE_BTOR2`, `MUNUNU_PROBE_ATOMS`, `MUNUNU_PROBE_GOOD`, `MUNUNU_PROBE_OP`, `MUNUNU_PROBE_DEPTH`, `MUNUNU_PROBE_RASTER`, `MUNUNU_PROBE_SECOND_SHADOW`, `MUNUNU_E1_ARM` | one-process-per-case reproduction probes (stack-depth, raster size, apply-call arms); exit 0 = survived, 101/134 = reproduced. |
| `MUNUNU_R3_UPDATE_BASELINE` (`=1`) | rewrite the R3 Kleene baseline fixture instead of comparing against it. |
| `MUNUNU_DIFF_TIER_EXPLORE` | the random-BTOR2 differential tier draws a fresh seed instead of replaying `TIER_SEED` (`make diff-tier-explore`); the gate run never sets it. |
| `MUNUNU_IC3_FRAMES`, `MUNUNU_IC3_REFINE`, `MUNUNU_IC3_QTO` | the IC3ia probe in `btor2/abs_safety.rs` (frame cap 40, refinement cap 24, per-query ms 8000); read only under `MUNUNU_INTERP_PROBE`. |
| `MUNUNU_INTERP_SUFFIX`, `MUNUNU_INTERP_ITERS`, `MUNUNU_INTERP_QTO`, `MUNUNU_INTERP_OVERALL` | the native interpolation probe in `btor2/native_interp.rs` (suffix depth 24, iterations 64, per-query ms 10 000, overall ms 60 000); read only under `MUNUNU_INTERP_PROBE`. |
