# Tutorial — V.6 AMBA arbiter (controllability-aware KMTS)

> **Status: in progress.** Tutorial covers the CLI path (shipped 2026-06-09).
> Web UI walkthrough is queued for the next R.6.7 session — see §"Web UI
> integration" below for the current state + the next-session plan.

> **Concept:** explaining what mununu does on this fixture — concept tag so
> `/docs-traceability` skips this file rather than requiring per-section
> Source-of-truth anchors.

## What this tutorial covers

How to run the **V.6 AMBA arbiter fixture** end-to-end on your local mununu
build to demonstrate the R.6.6 controllability-aware KMTS lift + R.6.3
modality-aware modal-step evaluation introduced by the R.6 controllability
track (2026-06-08 → 2026-06-09).

After this tutorial you should be able to:

1. Build the mununu CLI.
2. Lift the hand-authored AMBA arbiter BTOR2 to a controllability-aware KMTS.
3. Run a CEGAR refinement loop with the `--controllable` flag.
4. Read the JSON trace + understand what the verdicts mean.
5. Understand what's NOT yet wired up (UI workflow, full divergence demo).

## Prerequisites

- Rust toolchain matching `rust-toolchain.toml` (Edition 2024).
- Z3 installed (`brew install z3` on macOS, `apt install libz3-dev` on Debian).
  Set `LIBRARY_PATH=/usr/local/opt/z3/lib` (macOS Homebrew) when invoking cargo.
- Clone of `mununu` at `~/git_repo/mununu` (adjust paths if different).

## Step 1 — Build the CLI

```bash
cd ~/git_repo/mununu
LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli
```

This produces `./target/debug/mununu`. Sanity check the new flag:

```bash
./target/debug/mununu btor2 cegar --help | grep -A2 controllable-input
```

Expected output (one of the args lines):

```
--controllable <INPUT_NAME>
    R.6.6 / V.6 (2026-06-09) — name of a BTOR2 input symbol the
    controller drives. Repeated to declare multiple controllable inputs.
```

## Step 2 — Inspect the V.6 fixture

```bash
cat examples/verify/v6_controllability_kmts/source/amba_arbiter.sv | head -60
cat examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2
cat examples/verify/v6_controllability_kmts/README.md | head -80
```

Notable structure:
- **Verilog (`amba_arbiter.sv`)**: 2-client arbiter; 2-bit predicate-
  abstractable burst counter; explicit env/ctrl input split (`req_*`
  uncontrollable, `ctrl_*` controllable). Canonical documentation —
  not what the test consumes.
- **BTOR2 (`amba_arbiter.btor2`)**: hand-written equivalent of the SV.
  ~28 lines. Sidesteps the sv2v+Yosys subprocess requirement for the
  MVP test. This is what the CLI + integration tests consume.

## Step 3 — Run the CEGAR loop with the controllability-aware lift

The `--controllable` flag (new in 2026-06-09) tells the
predicate-cube lifter which BTOR2 inputs the controller drives. The
lifter then partitions boolean inputs into env (uncontrollable) + ctrl
(controllable) classes + emits per-combo dual-label transitions with
`LabelControllability::{Uncontrollable, Controllable}` tags.

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib ./target/debug/mununu btor2 cegar \
  examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2 \
  --formula 'nu X. < true > X' \
  --predicate 'burst_zero:burst=0' \
  --controllable ctrl_g0 \
  --controllable ctrl_g1 \
  --max-iterations 4
```

Expected output (last several lines):

```
CEGAR refinement loop completed
  fixture:           ...amba_arbiter.btor2
  formula:           nu X. < true > X
  predicate_source:  Wp
  iterations:        1
  terminated_with:   Converged
  final predicates:  1
```

The loop converges in 1 iteration on this trivial liveness probe
because every state in the lifted KMTS has at least one outgoing
edge (vacuous diamond witness).

### What's being computed

- **`predicate_cube_lift`** partitions the 4 boolean inputs (req_0,
  req_1, ctrl_g0, ctrl_g1) into 2 env-combos + 2 ctrl-combos =
  4 env-labels (`env_c0`–`env_c3`) + 4 ctrl-labels (`ctrl_c0`–`ctrl_c3`).
- Each lifted transition carries 2 labels: one env label + one ctrl
  label. The env labels are tagged `Uncontrollable`; the ctrl labels
  are `Controllable`.
- With the predicate set `{burst==0}`, the abstraction collapses
  `burst ∈ {1, 2, 3}` into one abstract state. Transitions from
  the `{¬burst==0}` cube are non-deterministic under the abstraction
  ⇒ emitted as `MayOnly`.
- The CEGAR loop's `evaluate_3v_game` evaluates the formula on the
  KMTS using the R.6.3 modality-aware modal step (the
  `TransitionModalityFilter::{All, MustOnly}` filter routes per
  (`ModalKind`, `must`|`may`)).

## Step 4 — JSON trace for downstream consumers

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib ./target/debug/mununu btor2 cegar \
  examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2 \
  --formula 'nu X. [ true ] X' \
  --predicate 'burst_zero:burst=0' \
  --controllable ctrl_g0 \
  --controllable ctrl_g1 \
  --max-iterations 4 \
  --json | python3 -m json.tool
```

Expected output (head):

```json
{
    "approximant_reuse_enabled": false,
    "final_predicate_count": 1,
    "fixture": "...amba_arbiter.btor2",
    "formula": "nu X. [ true ] X",
    "iterations": 1,
    ...
}
```

## Step 5 — Run the full V.6 validate.sh

The fixture ships a `validate.sh` script that runs both probes and
captures their outputs to a `build/` directory:

```bash
bash examples/verify/v6_controllability_kmts/validate.sh
```

Expected tail:

```
=== V.6 VALIDATION PASSED ===
outputs:
  ...build/cegar_liveness.out  — CEGAR loop log for νX. <true> X
  ...build/cegar_safety.json   — JSON CEGAR trace for νX. [true] X
```

The script also documents the next R.6.7 session items inline.

## Step 6 — Run the Rust integration tests

Five `#[test]` cases in `crates/mununu-core/tests/v6_controllability_kmts.rs`
exercise the lift programmatically + assert the R.6.6 done-criteria:

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib cargo test -p mununu-core --test v6_controllability_kmts
```

Expected: `test result: ok. 5 passed; 0 failed`.

The load-bearing test is **`v6_amba_arbiter_lifts_with_mayonly_transitions_present`** —
it asserts the lifted CLTS carries BOTH controllable labels AND MayOnly
edges from the same source. This is the R.6.7 done-criterion (R.6 plan §1).

## Web UI integration

**Status: MVP shipped 2026-06-09.** API extension (`/api/v1/context/import`
accepts `predicates` + `controllable` for BTOR2 input) + a
dedicated UI workflow panel mounted at the `/v6` route. SV-direct
input (UI runs sv2v + Yosys + lift internally) is the next session
item; today the user runs `mununu sv emit-btor2-per-module` first
to produce BTOR2 from SV, then pastes/uploads the BTOR2 in the UI.

### Backend API shape (shipped)

The `/api/v1/context/import` endpoint accepts two new optional
fields:

- `predicates: PredicateSpecRequest[]` — `{name, register, value}`
  triples.
- `controllable: string[]` — BTOR2 input symbol names the
  controller drives.

When both are non-empty AND `format == "btor2"`, the backend routes
through `predicate_cube_lift` with the R.6.6 controllability-aware
dispatch + returns a `ContextImportResponse` whose:
- `state_count` = cube count (= 2^|predicates|).
- `warnings` includes the lift's `AdapterWarning`s + a
  `[R.6.7 V.6 controllability-aware lift]` summary line counting
  mayonly / sharp / hyper_must / env_label / ctrl_label counts.
- `ctxdsl` is a comment-only summary CTXDSL (full Clts→CTXDSL emit
  is a follow-up).

Direct curl example:

```bash
curl -X POST http://localhost:8080/api/v1/context/import \
  -H 'Content-Type: application/json' \
  -d '{
    "content": "<paste full V.6 BTOR2 here>",
    "format": "btor2",
    "filename": "amba_arbiter.btor2",
    "predicates": [{"name": "burst_zero", "register": "burst", "value": 0}],
    "controllable": ["ctrl_g0", "ctrl_g1"]
  }'
```

### Step-by-step UI walkthrough

1. **Start the mununu HTTP API server** (from the mununu repo root):

   ```bash
   LIBRARY_PATH=/usr/local/opt/z3/lib cargo run -p mununu-cli -- serve
   ```

   Default port: 8080.

2. **Start the mununu-ui dev server** (from the mununu-ui repo root):

   ```bash
   cd ~/git_repo/mununu-ui
   npm run dev
   ```

   Default port: 5173.

3. **Open the V.6 workflow panel**: navigate to
   `http://localhost:5173/v6` in your browser.

4. **Paste the V.6 BTOR2**: open
   `examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2`
   in your editor (or `cat` it) and paste the contents into the
   "BTOR2 source" textarea. Alternatively click "Choose file" and
   select the `.btor2` file directly.

5. **Confirm the defaults**: the predicates field defaults to
   `burst_zero, burst, 0` (the V.6 fixture's predicate set); the
   controllable inputs default to `ctrl_g0` + `ctrl_g1` (the V.6
   fixture's controller inputs).

6. **Click "Run controllability-aware lift"**. The panel sends the
   request to the backend + renders the result.

7. **Inspect the lift summary**: expect to see:
   - Cube count: 2 (= 2^|predicates| for one predicate).
   - Warnings list containing the
     `[R.6.7 V.6 controllability-aware lift]` summary line — parse
     the inline metrics: mayonly / sharp / hyper_must / env_labels
     (should be 4) / ctrl_labels (should be 4).

   The summary CTXDSL is a comment block reproducing the same
   metrics; full Clts→CTXDSL emit is a follow-up.

### What the UI still needs (next session)

- **SV-direct input**: extend the panel to accept SV files + call
  the backend with `format: "sv-yosys"` which internally runs
  sv2v + Yosys before the predicate_cube_lift. Requires a parallel
  backend extension routing the SV path through
  `translate_sv_per_module` to get BTOR2 before lifting.
- **Full CTXDSL emit + graph rendering**: today the summary CTXDSL
  is a comment block. The next session extends `adapter::emit::emit`
  to accept a `Clts` directly (or a `Clts → AdapterIR` adapter),
  then the existing Monaco editor + cytoscape graph view render
  the result.
- **Property eval + verdict display**: integrate the CEGAR loop
  invocation so the panel can run a mu-calc property + show the
  verdict (Sharp / MayOnly / Unknown).

### How to track progress

- Master roadmap §11.4 V.6 sub-item 7: `~/.claude/plans/you-are-a-formal-vast-lake.md`.
- Surface parity skill: run `/parity-check` to verify the V.6 surface
  ships on CLI + API + UI. As of 2026-06-09 the V.6 surface is:
  - **CLI**: `mununu btor2 cegar --controllable ... --predicate ...` ✓
  - **API**: `POST /api/v1/context/import` with `predicates` +
    `controllable` (BTOR2-only today; SV-direct queued) ✓ MVP
  - **UI**: `V6ControllabilityAwareLiftPanel` at `/v6` ✓ MVP

## What's NOT in this tutorial (deferred)

- **End-to-end verdict-divergence demonstration** between pre-R.6.3
  modality-blind and post-R.6.3 modality-aware evaluation. The
  R.6.3 wire-in replaced the production verdict path, so the pre-R.6.3
  path is no longer reachable from `evaluate_tri`. The divergence is
  demonstrated on synthetic fixtures by the unit test
  `r6_3_evaluate_tri_mayonly_diamond_is_unknown_at_source` in
  `crates/mununu-core/src/mu_calculus/evaluator.rs` — that test proves
  the soundness fix on a 2-state KMTS.
- **GR(1) safety + liveness property authoring**. The probes used
  above (`νX. <true> X`, `νX. [true] X`) exercise the lift +
  modality-aware modal step; authoring formulas encoding "mutual
  exclusion" + "every request eventually granted" against the
  predicate cubes is the next V.6 session item.
- **SBY oracle cross-check.** The modality-aware verdict has no
  direct SBY equivalent; oracle comparison is a research follow-up
  per the R.6 plan §2 honesty discipline.

## Troubleshooting

- **`error: linker 'cc' failed: Z3 not found`** — set
  `LIBRARY_PATH=/usr/local/opt/z3/lib` (macOS) or the equivalent for
  your Z3 install.
- **`error: invalid value 'smt-per-target' for '--must-edge-inference'`** —
  the SMT must-edge variants ship in 2026-06-08+ commits; rebuild
  from the latest `main`.
- **CEGAR loop never converges** — try larger `--max-iterations`. If
  the loop hits the cap with `KleeneBot` cells, that's the R.5 CEGAR
  bounded-refinement signal; per the §10.1 R.5 done-criterion this
  is reported via `terminated_with: BoundedIterationsReached`.

## See also

- Fixture README: `examples/verify/v6_controllability_kmts/README.md`
- R.6 replanning plan: `~/.claude/plans/r6-controllability-aware-kmts-game-abstraction.md`
- Master roadmap §11.4 R.6 sub-track: `~/.claude/plans/you-are-a-formal-vast-lake.md`
- R.6.6 lifter implementation: `crates/mununu-core/src/adapter/btor2/kmts_lift.rs:predicate_cube_lift`
- R.6.3 modality-aware modal step: `crates/mununu-core/src/mu_calculus/evaluator.rs:eval_node_tri`
- KMTS theory + the §7.2 rule table: `docs/design/kmts-theory.md`
- V.6 industrial-value entry: `docs/design/industrial-value-and-validation-domains.md` §8.5
- V.6 proof-by-fire ledger row: `docs/design/proof-by-fire-findings.md` row 5
