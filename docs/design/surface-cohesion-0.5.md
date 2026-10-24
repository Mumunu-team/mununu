# Surface cohesion review for 0.5 — verbs, flags, verdicts, traces

> Status: planning — APPROVED as written by the maintainer on 2026-10-23 (every **Decide** below is accepted); the HTTP API stays under `/api/v1`. Nothing here is shipped until each cluster lands.

**Audience:** the maintainer deciding the 0.5 break, and the engineers implementing it in mununu, rosf, monono and mununu-ui.

**Why now.** 0.5 is the release EmTech's tutorial videos will be recorded on, and it is a declared clean break (no aliases, consumers migrate the same week). Every surface that is renamed later costs another coordinated migration, and every new surface planned for 0.5 (`--template`, `--emit-witness`, `--emit-testbench`, `sv cdc-lint`, `check-fsm --liveness`, `context eval --sanity`, `fairness weak`, `allow_dead_vectors`, the deployment config) should be born under the conventions decided here.

**Method.** The CLI tree (`crates/mununu-cli/src/main.rs`), the routes (`crates/mununu-core/src/api/server.rs`), the wire types (`models.rs`) and the schemas (`docs/api-schemas/`) were read; the consumers' call sites were read in rosf (`crates/rosf-core/src/verify/mununu.rs`, `examples/cheatsheet/`), monono (`verify/*.sh`) and mununu-ui (`src/api/endpoints.ts`). Line numbers are as of `8b98949`.

## 1. What a consumer sees today

mununu exposes 11 visible top-level commands (plus the hidden `internal-engine-eval`) and 42 routes under `/api/v1`. One shared verdict vocabulary exists — `PropertyVerdict` = `holds | violated | unknown | skipped` (`verdict.rs`) — and the RTL property verbs use it. The CTXDSL verbs, the game and the synthesis paths do not, and counterexamples come in seven shapes. Exit codes overlap between a verdict and a usage error.

## 2. Findings and proposals

### 2.1 Exit codes

| # | finding | evidence |
|---|---|---|
| E1 | `2` means VIOLATED and also a clap usage error (`Cli::parse()` exits 2); rosf misread a bad flag as a violation (mununu#541) | `main.rs:2892`, `ci_exit_code` `main.rs:215` |
| E2 | `sv lint` uses `2` for "found something" | `main.rs:1907` |
| E3 | `context eval` always exits 0, whatever the verdict | `main.rs:8040-8080` |
| E4 | `btor2 game --fault` maps `realizable` to `violated` (polarity flips with a flag) | `main.rs:3628` |

**Proposal (Decide).** One table for every verb: `0` pass, `1` tool error, `2` violated, `3` unknown under `--fail-on unknown`, `4` expectations unmet, `64` usage error (clap error kind mapped explicitly, `EX_USAGE`). `sv lint` and `check-fsm` get `--fail-on` like the property verbs. `context eval` gets the same CI exit. The game keeps its flip but states it in the verdict envelope (§2.3) rather than only in the exit code.

### 2.2 Verb tree

| # | finding | evidence |
|---|---|---|
| V1 | three safety verbs: `btor2 verify` (portfolio), `btor2 verify-safety` (cube; no API route, no `sv` peer), `sv verify` (bad-reachability of `assert` lowered by yosys) next to `sv verify-auto` (every SVA property) | `main.rs:511,579,1880` |
| V2 | four liveness verbs: `verify-liveness`, `-all`, `-under-fairness` (equal to `verify-liveness` with an empty list), `verify-justice`; `sv` has no fairness or justice verb | `main.rs:533-561` |
| V3 | `context eval` (prose), `verify` (`verify.toml`, `satisfied` bool) and `POST /context/verify` (no CLI verb of that name) answer the same question three ways | `main.rs:8057`, `models.rs:544` |
| V4 | CLI and route names differ: `context synth` ↔ `/context/synthesize`, `context graph` ↔ `/context/graphs`, `memory check` ↔ `/verify/memory-check`, `context synth --controller-mode gr1` ↔ `/synth/gr1` | `server.rs:47-60,206` |
| V5 | `cegar` exists under both `btor2` and `sv`; neither response is schema-pinned | `verify-verbs.md` |

**Proposal (Decide).**
- `btor2 verify-liveness` absorbs `-all` (`--response A=>C` repeatable), `-under-fairness` (`--fairness`) and `verify-justice` (`--justice`, `--rung`); the `sv` side gets the same flags. Four verbs become one per family.
- `btor2 verify-safety` gets an API route or is removed (it duplicates a portfolio member).
- `sv verify` is renamed `sv verify-bad` (it checks yosys-lowered `bad` lines, not the SVA fragment), so "verify" without a suffix means the full property set only in `sv verify-auto`. Alternative: remove it.
- `context eval` gets `--json` emitting the same envelope as `/context/verify`; the route is renamed `/context/eval` to match. `/context/synthesize` → `/context/synth`, `/context/graphs` → `/context/graph`, `/verify/memory-check` → `/memory/check`; `/synth/gr1` folds into `/context/synth` with `controller_mode: "gr1"`.

### 2.3 Verdict envelope

| # | finding | evidence |
|---|---|---|
| R1 | `btor2 game` reports `realizable` + `attack_exists`; `context synth` and `/synth/gr1` report `realizable` printed as yes/no; `context eval` prints "Initial states satisfying: k/N"; `verify` reports `satisfied`; `cegar` reports cell counts | `models.rs`, `main.rs` |
| R2 | `btor2 verify --help` still says it prints `reachable / unreachable / contradiction`; the code prints `holds / violated` | `main.rs:506` vs `:4218` |
| R3 | `sv verify-liveness*` emit `Sv*Report` on the CLI and `Btor2VerifyLivenessResponse` on the API | `handlers.rs:1368,1423,1476` |
| R4 | no schema for game, synth, eval, justice, cegar | `docs/api-schemas/` |

**Proposal (Decide).** One per-property envelope on every deciding verb, CLI and API alike:

```json
{ "name": "...", "label": "...", "kind": "assert|assume|cover|objective|formula",
  "outcome": "holds|violated|unknown|skipped",
  "decided_by": "exact-symbolic|...", "detail": "...",
  "bottom_reason": {...}, "vacuity": {...}, "trace": {...} }
```

The game and synthesis report `outcome` for the stated objective (`holds` = realizable for the controller) and keep `realizable` as a field; under `--fault` the objective is the attacker's, which the envelope's `kind: "objective"` and `detail` say explicitly. `context eval` reports `holds` iff every initial state satisfies, with `satisfying` / `violating` counts as fields. Schemas are added for every response that lacks one; the CLI and API share one type per verb.

### 2.4 Traces

Seven shapes today: `CounterexampleView` (`prefix`/`cycle`/`inputs`/`unreachable_target`), `btor2 verify --witness` (`{depth, states, inputs}`, CLI only), the fault attack (`{depth, faults[]}` on the CLI, `[FaultEventView]` on the API), cegar `CounterTraceView`, game `TwoPlayerStrategy` + `StallLassoView`, CTXDSL `LassoTraceApi` (string lists), `CounterstrategyResult` (Cytoscape graph).

**Proposal (Decide).** One `Trace`:

```json
{ "kind": "finite|lasso|trap|unreachable|attack",
  "steps": [ { "cycle": 0, "state": [{"name": "...", "value": "...", "bits": 3, "hex": "..."}],
               "inputs": [...], "faults": [{"site": "...", "value": "..."}], "labels": ["..."] } ],
  "loop_start": 3, "unreachable_target": ["..."], "replay": { "vcd": "...", "testbench": "..." } }
```

RTL rows fill `state`/`inputs`/`faults`; CTXDSL rows fill `state` (state names) and `labels`. A strategy stays its own object (`strategy: {kind: controller|environment, rows: [...]}`), and the Cytoscape graph stays a view endpoint. `replay` is where P0.7's `--emit-witness` / `--emit-testbench` outputs attach.

### 2.5 Flags

| # | finding | evidence |
|---|---|---|
| F1 | `--config-value SIG=V` pins (verify-auto, mutate); `--config-values NAME=v1,v2` enumerates a partition (recoverability, cegar); the API uses `config_values` for both | `main.rs:2235,2468,841,1168`; `models.rs` |
| F2 | `--predicate EXPR` (verify-auto) vs `--predicate NAME:REG=VALUE` (recoverability, cegar) | `main.rs:2286,2451,1146` |
| F3 | `sv verify-auto` has its own lift flags (no `--design-dir`, `--search-path`, `--exclude`); `SvLiftArgs` lacks `--param`, `--lift-override` | `main.rs:2117` vs `2314` |
| F4 | front end: CLI `--frontend auto|verilog|slang` + `--preprocess-sv2v`; API `use_slang` / `use_sv2v` booleans (no way to ask for `verilog`); `context --preprocessor` | `main.rs:726`; `models.rs:1318` |
| F5 | `--json` is absent on `sv verify`, `check-fsm`, `lint`, `mutate`, most `btor2` verbs, `context eval` | `main.rs:86` |
| F6 | `--expect-*` / `--fail-on` only on verify-auto; `verify` uses `--strict` | `main.rs:2308,1363` |
| F7 | timeouts: `--timeout-ms`, `--owned-timeout-ms`, `--budget-ms`, otherwise env vars | `main.rs:968,973,1056` |
| F8 | `--controllable` (game) vs `--controllable-input` (cegar) | `main.rs:1226,869` |
| F9 | API spellings drift from the CLI: `gate_reset` vs `--no-gate-reset`, three `rescue_bottom_*` keys vs `--no-rescue`, `cutpoint` (singular) beside `counter_bounds` (plural), `lift_overrides` carries file contents where the CLI takes paths | `models.rs:1762-1930` |

**Proposal (Decide).**
- `--pin SIG=V` (fixed value) and `--partition NAME=v1,v2` (enumerated); API `pins` / `partitions`.
- `--predicate` takes `EXPR` everywhere; the `NAME:` prefix becomes optional (`NAME:EXPR`).
- One `SvLiftArgs` flattened into every `sv` verb (all of: file / `--design-dir`, `--source`, `--top`, `--include-dir`, `--search-path`, `--exclude`, `--frontend`, `--param`, `--lift-override`, `--cutpoint`, `--merge-clock-domains`); the API mirrors it as one `lift` object.
- `--frontend auto|verilog|slang|sv2v` (sv2v as a front end, not a separate switch); API `frontend` string.
- `--json`, `--fail-on`, `--expect*` on every deciding verb; `--strict` becomes `--fail-on unknown`.
- `--timeout-ms` is the one wall-clock flag; engine budgets stay environment variables (they change verdict determinism, see `MUNUNU_BDD_TIME_BUDGET_MS`).
- `--controllable` everywhere.
- API keys are the CLI flag names in snake_case, positive form (`gate_reset: false` for `--no-gate-reset`, `rescue: false`), plural for repeatable flags (`cutpoints`).

### 2.6 Asymmetries

CLI-only: `btor2 discover|lift-kmts|inject-fault|from-aiger|verify-safety`, `sv preprocess|emit-btor2-per-module|validate|discover`, `context merge`, `contract gaps|sidecars`, `codesign couple|import-svd|extract-c|emit-cmsis-header`, `library`, `extraction check`, `btor2 verify --witness`. API-only: `/context/import`, `/extraction/{domains,composition-modes,propose-composition,extract}`. The UI's `Btor2VerifyResponse` lacks `fault` / `target` / `attack`.

**Proposal (Decide).** Each CLI-only verb either gets a route or a `surface: CLI-only — <reason>` declaration in its doc anchor (the parity rule's escape); the review's default is a route for `inject-fault` and `--witness` (folded into `trace.replay`), and the declaration for the developer-facing verbs (`emit-btor2-per-module`, `preprocess`, `lift-kmts`).

## 3. Conventions for the surfaces 0.5 adds

| new surface | convention |
|---|---|
| `sv verify-auto --template ID --template-arg K=V` | rows labelled `tmpl:<id>[K=V,…]`, envelope as §2.3; API `templates: [{id, args}]` |
| `--emit-witness DIR`, `--emit-testbench DIR` | populate `trace.replay`; API returns contents inline (no server paths) |
| `sv cdc-lint` | `--json`, `--fail-on`, exit table §2.1; findings as rows with `outcome` (`violated` = an unsafe crossing) |
| `check-fsm --liveness [--terminal REG=V]` | rows per register and state, `outcome` + `kind: "inventory"` for the terminal-states list |
| `context eval --sanity` | rows in the same envelope (`kind: "sanity"`) |
| composition `fairness weak`, `allow_dead_vectors` | DSL keywords; API mirrors on the context request |
| deployment config (`mununu.toml`) | `--deployment-config` / `MUNUNU_DEPLOYMENT_CONFIG` / `server --config` |

## 4. Consumer impact

| change | rosf (adapter: `sv verify-auto` only; cheatsheet: 5 verbs) | monono | mununu-ui |
|---|---|---|---|
| exit 64 for usage errors | fixes the #541 misread; adapter treats 64 as `skipped` like 1 | `verify/lib.sh` exit handling | — |
| verdict envelope | additive on verify-auto (fields exist); cheatsheet `"verdict"` keys on recoverability / check-fsm / liveness → `outcome` | `verify --json` `property_verdicts[].satisfied` → `outcome`; `context eval` greps → `--json` | `verdict`, `realizable`, `outcome` reads on ~40 routes |
| `Trace` | not read by the adapter; cheatsheet reads `counterexample` → `trace` | — | `stall_lasso`, `strategy.kind`, lasso viewers |
| `--pin` / `--partition` | adapter passes `--config-value` → `--pin`; probe strings in `--help` updated | lane scripts | API key rename |
| shared `SvLiftArgs` / `--frontend sv2v` | adapter passes `--frontend slang` (unchanged) | — | request shape (`lift` object) |
| liveness verb merge | cheatsheet `verify-liveness` unchanged; `-all` → flag | comments only | route rename |
| route renames | not an API consumer | not an API consumer | typed client in `endpoints.ts` |
| `context eval --json` | — | replaces the "Initial states satisfying: 1/1" greps | — |

rosf pins mununu 0.4.0 (`toolchain.lock:37`) and probes every flag it passes in `--help` (`mununu.rs:82-145`); the 0.5 migration unfreezes the adapter, updates the probes and recaptures `tests/fixtures/captured/mununu-<0.5>/`. The rosf lanes parked on mununu#541 (recoverability, game) can be revisited because those reports are pinned now.

## 5. Not decided here

- (Decided 2026-10-23) The routes stay under `/api/v1`; the 0.5 migration briefing documents every renamed route and changed shape.
- Deprecation machinery for later releases (no aliases exist today; 0.5 is a clean break by decision).
