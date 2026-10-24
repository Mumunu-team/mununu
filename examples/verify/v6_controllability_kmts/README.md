# V.6 — R.6.7 controllability-aware KMTS proof-of-fire

> **Status: PASS (partial — Option B fixture, RTL path).** First session
> ship: hand-authored AMBA-style arbiter Verilog + equivalent BTOR2 +
> Rust integration test demonstrating the R.6.6 controllability-aware
> lifter + R.2.5 predicate-image MayOnly emission end-to-end. The
> verdict-divergence demonstration (modality-blind vs modality-aware
> evaluation) follow-up is queued for the next R.6.7 session.

## Context

R.6.7 ships the V.6 proof-of-fire industrial milestone for the R.6
controllability-aware KMTS arc (R.6.1 theory → R.6.2 draft → R.6.3
production wire-in → R.6.4 hyper-must cardinality → R.6.5 owning-player
tag → R.6.6 controllability-aware adapter → **R.6.7 industrial demo**).

### Path-chosen rationale

Per the 2026-06-09 fixture-path analysis (recorded in this session's
chat log + the master roadmap §11.4 "Slot-order discipline"), the
R.6 plan §2 primary candidate — public AMBA AHB from the
SYNTCOMP/TLSF corpus — was found to require infrastructure mununu
does NOT have:

- The mununu TLSF adapter (`crates/mununu-core/src/adapter/tlsf/mod.rs`)
  goes directly TLSF → CTXDSL via the shared IR. It produces a
  Sharp-only CLTS by construction.
- The path to a KMTS with predicate-abstraction-induced MayOnly edges
  requires **BTOR2 as input** (the only path that emits MayOnly is
  `crates/mununu-core/src/adapter/btor2/kmts_lift.rs:predicate_cube_lift`).
- TLSF → BTOR2 (with a predicate-abstractable datapath) is NOT a
  built-in mununu capability. The R.6 plan §2 implicitly assumed
  TLSF → RTL (Verilog) → Yosys → BTOR2; the TLSF → RTL link
  requires an external GR(1) synthesiser (Strix / BoSy / similar)
  that produces the controller mununu is supposed to *verify*, not
  consume.

This V.6 fixture is **Option B** from the path-chosen menu: a
hand-authored Verilog implementation of an AMBA-style arbiter with
a small predicate-abstractable burst counter. The Verilog is the
canonical documentation (`source/amba_arbiter.sv`); the equivalent
BTOR2 (`source/amba_arbiter.btor2`) is what the integration test
consumes to sidestep the sv2v + Yosys subprocess requirement for
the V.6 MVP test (the test still verifies the actual R.6.6 +
R.6.3 code paths end-to-end).

### Honest claim (per CLAIM Integrity)

This is a **hand-authored Verilog/BTOR2 fixture**, NOT the public
AMBA AHB IP from the SYNTCOMP corpus. It demonstrates the
controllability-aware verdict-divergence pattern that the
R.6.3/4/5/6 evaluators are designed to produce, on a small RTL
fixture exercising the same code paths that would run on a real
SYNTCOMP/AMBA fixture if the TLSF → BTOR2 extraction infrastructure
existed.

**Soundness update (2026-06-23 — PO-3 / R.6.8 CLOSED).** The
controllability-aware *definite* verdict is now sound. The per-player
audit (`.claude/reviews/cube-modal-soundness/`) caught and fixed a
two-pass over-claim, and `evaluate_tri` now routes
`Control::{Controllable, Environment}` through the de Alfaro–Godefroid–
Jagadeesan per-player rule (`modal_trit_from_target`). Previously a
*definite* controllability verdict had to be labelled "unaudited /
design-pattern demonstration"; that caveat is lifted. V.6's verdicts
are unchanged by the fix (its demo rests on a conservative `KleeneBot`,
which was already sound). See [`kmts-theory.md`](../../../docs/design/kmts-theory.md)
§7.5 (Resolution 2026-06-23).

## Fixture design

### Verilog (`source/amba_arbiter.sv`)

- **Module**: `amba_arbiter`. 2-client arbiter with a 2-bit burst
  counter. Synthesisable (sv2v + Yosys friendly).
- **Inputs**:
  - `req_0`, `req_1` — client requests (uncontrollable / environment).
  - `ctrl_g0`, `ctrl_g1` — per-cycle controller grant choices
    (controllable / system; named with `ctrl_` prefix so the R.6.6
    sidecar's `controllable_inputs` list is unambiguous).
- **State**:
  - `burst` (2 bits) — the predicate-abstraction target.
  - `grant_0`, `grant_1` (1 bit each) — registered controller decisions.
- **Logic**:
  - `grant_0' = ctrl_g0`, `grant_1' = ctrl_g1` (pass-through).
  - `burst' = (ctrl_g0 || ctrl_g1) ? (burst==0 ? 3 : burst-1) : burst`.
    The burst counter ticks down per cycle when a grant is active;
    re-arms to 3 when burst==0.

### BTOR2 (`source/amba_arbiter.btor2`)

Hand-written equivalent of the Verilog. ~28 lines; matches the
SV semantics line-for-line via `next`/`init` declarations.

### Predicate abstraction (test setup)

The integration test (`crates/mununu-core/tests/v6_controllability_kmts.rs`)
runs `predicate_cube_lift` with:

- `predicates = [{name: "burst_zero", register: "burst", value: 0}]`
  — a single predicate `burst == 0`. With this predicate set, the
  abstraction collapses `burst ∈ {1, 2, 3}` into one abstract state
  `{¬burst==0}` whose successors are non-deterministic under the
  abstraction (depends on whether the concrete burst was 1, 2, or 3).
  This is what introduces MayOnly transitions.
- `adapter_options.controllable_inputs = ["ctrl_g0", "ctrl_g1"]` —
  the R.6.6 controllability split.
- `max_input_bits = 8` — enumerate all 2^4 = 16 input combos
  (req_0, req_1, ctrl_g0, ctrl_g1).

The R.6.6 lifter partitions inputs: 2 env-combos × 2 ctrl-combos
= 4 env-combo labels + 4 ctrl-combo labels emitted, with appropriate
`LabelControllability::{Uncontrollable, Controllable}` tags.

## What's tested (V.6 done-criteria per R.6.7)

The integration test
`crates/mununu-core/tests/v6_controllability_kmts.rs` has 5 cases:

1. **`v6_amba_arbiter_lifts_with_controllability_aware_dual_labels`** —
   the lifter emits 4 `env_c*` Uncontrollable labels + 4 `ctrl_c*`
   Controllable labels (R.6.6 dispatch fires).
2. **`v6_amba_arbiter_lifts_with_mayonly_transitions_present`** —
   the load-bearing R.6.6 done-criterion: the lifted CLTS contains
   MayOnly transitions AND controllable labels from the same source.
   Without this, R.6.3/4/5 reduce to pre-R.6.3 paths and the V.6
   verdict-divergence pattern cannot fire.
3. **`v6_amba_arbiter_lift_produces_expected_cube_count`** — sanity:
   single predicate ⇒ 2 cubes.
4. **`v6_amba_arbiter_controllability_aware_skips_smt_post_pass`** —
   R.6.6 gate correctly skips SmtPerTarget promotion under
   controllability-aware mode.
5. **`v6_amba_arbiter_without_controllability_preserves_legacy_lift`** —
   verdict-equivalence baseline: empty `controllable_inputs` yields
   the legacy single-`step` label shape (pre-R.6.6 behaviour
   preserved bit-for-bit).

All 5 PASS.

## What's NOT tested (deferred)

- **End-to-end verdict-divergence** between pre-R.6.3 modality-blind
  and post-R.6.3 modality-aware evaluation. The R.6.3 wire-in
  replaced the production verdict path, so the pre-R.6.3 path is no
  longer reachable from the current `evaluate_tri`. The divergence
  is demonstrated on synthetic fixtures by the unit test
  `r6_3_evaluate_tri_mayonly_diamond_is_unknown_at_source` in
  `crates/mununu-core/src/mu_calculus/evaluator.rs` — that test
  proves the soundness fix on a 2-state KMTS; this V.6 fixture
  shows the same evaluator path is exercised on a real-RTL-derived
  KMTS. Bringing the divergence into a single end-to-end V.6 test
  would require either a `--evaluate-modality-blind` CLI flag or a
  feature-flagged restore of the pre-R.6.3 path — a strict-additive
  follow-up.
- **CLI invocation** via `mununu btor2 cegar --controllable`.
  The CLI flag for `controllable_inputs` doesn't yet exist on the
  BTOR2 subcommand (R.6.6 lifter reads it from
  `AdapterOptions::controllable_inputs` which today is only
  populated by sidecar resolvers for SV/AIGER/XState). A CLI flag
  is a strict-additive follow-up; per CLAUDE.md §Surface Parity
  this should ship before V.6 is considered "fully closed".
- **GR(1) safety + liveness property verdicts**. The integration
  test exercises the lift + the modal-step semantics; authoring a
  mu-calculus formula encoding "mutual exclusion" + "every request
  eventually granted" against the cube space + evaluating it
  end-to-end is the next R.6.7 session.

## Re-running

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib \
  cargo test -p mununu-core --test v6_controllability_kmts
```

Expected output: `test result: ok. 5 passed`.

## See also

- R.6 replanning plan: `.claude/plans/r6-controllability-aware-kmts-game-abstraction.md`
- Master roadmap §11.4 R.6 sub-track: `.claude/plans/you-are-a-formal-vast-lake.md`
- R.6.6 lifter implementation: `crates/mununu-core/src/adapter/btor2/kmts_lift.rs:predicate_cube_lift`
- R.6.3 modality-aware modal step: `crates/mununu-core/src/mu_calculus/evaluator.rs:eval_node_tri`
- V.6 entry in industrial-value-and-validation-domains.md: `docs/design/industrial-value-and-validation-domains.md` §8.5
