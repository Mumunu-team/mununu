# Predicate-Cube CEGAR

> **Source of truth:** [`adapter::btor2::cegar::cegar_refine_loop`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/btor2/cegar.rs), [`adapter::btor2::kmts_lift::predicate_cube_lift`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/btor2/kmts_lift.rs), [`mu_calculus::evaluate_tri`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/mu_calculus/evaluator.rs) — surface: CLI+API+UI (`mununu btor2 cegar` / `mununu sv cegar`, `POST /api/v1/btor2/cegar` / `POST /api/v1/sv/cegar`, the `/cegar` panel).

Predicate-cube CEGAR is mununu's **second RTL verification engine**. Where the
[RTL Verification Pipeline](RTL-Verification-Pipeline) bit-blasts the design into
an explicit state space (exact, but capped at ~2^18 states, single module), this
engine abstracts the design into a **predicate cube** — abstract states are
truth-assignments to a small set of Boolean predicates over the design's
registers — and decides properties with a **3-valued (Kleene) mu-calculus** over a
**Kripke Modal Transition System (KMTS)**, refining the predicate set with **CEGAR**
when the abstraction is too coarse. It is the path for designs whose register
width blows the bit-blast cap.

## When to use which engine

| | [Bit-blast pipeline](RTL-Verification-Pipeline) | Predicate-cube CEGAR (this page) |
|---|---|---|
| Abstract state space | `2^(register bits)` (≤ 2^18 cap) | `2^(predicate count)` — independent of register width |
| Verdict | 2-valued (`true` / `false`) | 3-valued (`KleeneT` / `KleeneF` / `KleeneBot`) |
| Refinement | manual (edit the sidecar) | automatic (CEGAR adds predicates) |
| Best for | small FSM-heavy control logic | wide-datapath / large-register designs, security hazards |

## Pipeline

```
SystemVerilog ──sv2v──► Verilog ──Yosys (no flatten)──► BTOR2
   │                                                      │
   └────────── (mununu sv cegar wraps this) ──────────────┘
                                                          ▼
                          predicate_cube_lift  (BTOR2 → KMTS over 2^|P| cubes)
                                                          ▼
                       3-valued mu-calculus  (evaluate_tri: KleeneT/KleeneF/KleeneBot)
                                                          ▼
                       CEGAR  (on KleeneBot: add a predicate, re-lift, re-evaluate)
                                                          ▼
                          verdict cells:  T=… F=… ⊥=…
```

`mununu btor2 cegar` consumes a BTOR2 file directly; `mununu sv cegar` is the
one-call variant that runs sv2v + Yosys for you.

### Explicit vs symbolic engine (R-F5)

The predicate-cube abstraction has two possible **engines** — the representation of the
cube state space + transition relation, independent of the abstraction itself:

- **Explicit (default)** — materialises `2^|P|` cube states in a `Clts` and builds the
  may/must edges with SMT (`SmtEncode`). Fine at small `|P|`; the edge computation is
  `O(2^2|P|)` SMT queries, which is the scaling wall. Supports the full CEGAR refinement
  loop, `--predicate-source`, sidecar compound predicates, etc.
- **Symbolic / BDD (R-F5, shipped — opt-in via `--engine symbolic`)** — represents cube
  sets + the transition relation as BDDs (bit-blasted straight from the BTOR2 transition
  function) and runs the fixpoint by image/preimage (`∃x'. R(x,x') ∧ φ(x')`), never
  enumerating cube pairs or issuing per-cube-pair SMT. Orders of magnitude faster at large
  `|P|` (≈10 ms at `|P|=12`, where the explicit SMT path issues ~16.7M queries / ~29 min).
  The `evaluate_tri` verdict is the ground truth either way — the symbolic path is
  validated cell-for-cell against it. Runs the **CEGAR refinement loop** (WP predicate
  discovery on ⊥, rebuilding the BDD relation each iteration — still no per-cube-pair SMT;
  `--max-iterations 0` = single-shot). **Current scope (R-F5.5a/b/c):** simple
  `NAME:REG=VALUE` equalities + non-derived `--sidecar` `compound_predicates` (`cnt >= 2`,
  relational), and the `[]`/`<>` fragment including modalities guarded by
  `req_cur`/`forb_cur`/`req_next`/`forb_next` state predicates (R-F5.5c). Label,
  controllability (`ctrl`), and step-bounded (`steps`) guards remain out-of-fragment over a
  predicate cube (an honest error — see `cube_modality_soundness_warnings`).
  Derived/combinational predicates and the Clts-only optimisations (failure-subgame
  precision, approximant reuse, CTXDSL emit) remain `--engine explicit` only. Source of
  truth:
  [`adapter::btor2::symbolic_engine::symbolic_cegar_refine`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/btor2/symbolic_engine.rs).
- **Exact symbolic MC (D1, shipped — `sv verify-auto --engine exact-symbolic` only)** — *not*
  a predicate-cube engine: it drops the abstraction entirely and decides the μ-calculus
  **exactly** over the full bit-blasted state by ROBDD μ/ν fixpoint from the reset init.
  Because the state is exact (no may/must split), every verdict is **definite** — 2-valued
  `Holds` / `Violated`, never ⊥. This decides `AF`-liveness (and any μ-calculus property)
  where *both* cube engines return Unknown: over the exact finite state the least-fixpoint
  iteration **is** the ranking, so no ranking/fairness infrastructure is needed. Bounded by
  BDD size — a design too large to bit-blast makes the property `Skipped` (graceful, no OoM).
  Exposed only on `sv verify-auto` (the surface that supplies a reset-gated model). Source of
  truth:
  [`adapter::btor2::symbolic_bitblast::exact_symbolic_verdict`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs)
  — surface: CLI+API+UI.

The two cube engines share the same 3-valued semantics and soundness (below). See
[the post-R-F5 architecture](https://github.com/Mumunu-team/mununu/blob/main/docs/design/post-rf5-architecture.md)
for the full picture (IR layering, may/must edges, over/under/⊥ approximation).

## CLI

> **Source of truth:** the `btor2 cegar` / `sv cegar` subcommands in [`crates/mununu-cli/src/main.rs`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-cli/src/main.rs) — surface: CLI.

```bash
mununu btor2 cegar <design.btor2> \
  --formula '<> (p5 || p6 || p7)' \        # the mu-calculus property
  --predicate p5:boot_fsm_ns=5 \           # NAME:REGISTER=VALUE (repeatable)
  --predicate p6:boot_fsm_ns=6 \
  --predicate p7:boot_fsm_ns=7 \
  --config-values 'boot_fsm_ns=0,1,2,3,4,5,6,7' \  # admit these power-up values
  --must-edge-inference smt-per-target              # prove must-edges with Z3
```

Key flags:

| Flag | Purpose |
|---|---|
| `--formula` | the mu-calculus property (see [Mu-Calculus Reference](Mu-Calculus-Reference)) |
| `--predicate NAME:REG=VALUE` | one cube predicate (`REG == VALUE`); repeatable |
| `--config-values 'REG=v1,v2,…'` | admit an under-constrained register's power-up values (R-Y7 symbolic init) |
| `--must-edge-inference` | `off` (sampling) \| `smt-per-target` (∀∀, Z3-proved) \| `smt-per-target-standard` (∀∃) \| … |
| `--may-edge-inference` | `off` (sampling) \| `smt-all-pairs` (sound ∃ over-approximation) |
| `--max-iterations` | CEGAR refinement cap (default 16) |
| `--controllable-input` | mark an input as controller-chosen (synthesis / controllability-aware lift) |
| `--emit-ctxdsl <PATH>` | dump the refined cube model as CTXDSL |
| `--engine` | `explicit` (default, SMT edges + CEGAR) \| `symbolic` (R-F5 BDD relation + CEGAR loop, no per-cube-pair SMT — orders of magnitude faster at large `\|P\|`; handles equality + non-derived sidecar compound predicates) |

`mununu sv cegar <design.sv> --formula … --predicate …` accepts the same property
flags and runs sv2v + Yosys internally.

## 3-valued verdicts

Each cube gets one of three verdicts, and the CLI prints the tally as
`verdict cells: T=… F=… ⊥=…`:

- **`KleeneT`** — the property is *definitely true* on this cube. By the KMTS
  preservation theorem (Bruns–Godefroid CONCUR 2000) a definite verdict
  **transfers to the concrete RTL**.
- **`KleeneF`** — *definitely false* on this cube; also transfers.
- **`KleeneBot` (⊥)** — the abstraction is **too coarse to decide**. This is not a
  failure: it is the explicit "refine me" signal. CEGAR adds a predicate and
  retries; if it cannot, `⊥` is the sound, honest answer (the design may be safe
  or not — this cube can't tell).

## Soundness — the audited-sound fragment

> **Source of truth:** [`mu_calculus::cube_modality_soundness_warnings`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/mu_calculus/mod.rs) — surface: CLI+API+UI (the cegar `warnings` channel). Full write-up: [`docs/design/predicate-abstraction-recipe.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/predicate-abstraction-recipe.md) §4.9.

A definite verdict transfers soundly for the fragment the cube path actually
evaluates: **bare (label-agnostic), single-agent (`Control::All`), unbounded**
modalities — i.e. plain `<>` and `[]`. For a synchronous design this is the
*complete* modal vocabulary (one clock = one step, with the input quantified
inside the modality), so M.4-style properties like `<> (p5 || p6 || p7)` are
exactly in fragment. The soundness chain — the lift is a sound may/must KMTS, and
the evaluator computes the standard 3-valued modal semantics — is mechanised in CI.

Modal forms **outside** that fragment are not silently answered — the tool emits a
soundness warning in the `warnings` channel:

| Out-of-fragment form | Warning |
|---|---|
| controllability `<(ctrl=controllable)>` / `[(ctrl=environment)]` | per-player game semantics is **unaudited** (awaits the R.6.8 per-player audit) — not a sound *definite* controllability verdict |
| bounded `<(steps=k)>` | the may/must filter is not applied to bounded steps — 3-valued soundness not established |
| label-specific `<a>` on a non-cube label | **vacuous** — the cube collapses every concrete action onto its own label |

For example, running a `<(ctrl=controllable)>` property over a cube prints:

```
warnings:
  - adapter/btor2/cegar (PO-2 cube-modal soundness): <> modality with
    ctrl=Controllable over a predicate cube: the per-player (controller ×
    environment) game semantics is UNAUDITED (PO-3 / R.6.8; …) — this is NOT a
    sound definite controllability verdict. …
```

## Worked example — Caliptra boot-FSM (CWE-1245)

> **Runnable:** [`examples/verify/sv_yosys_caliptra_rtl_150/validate_m4_cegar.sh`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/sv_yosys_caliptra_rtl_150/) (requires `yosys` + `sv2v` + `z3` on `PATH`). This is mununu's M.4 milestone — full automated CEGAR on real industrial RTL.

The Caliptra `soc_ifc_boot_fsm` holds its state in a 3-bit `boot_fsm_state_e`
register with five legal encodings (0–4). The bug-bearing `pre_fix` variant has a
`unique casez` with **no `default` arm**, so the type-admissible-but-unhandled
encodings {5, 6, 7} latch (MITRE **CWE-1245**). The `post_fix` variant adds a
`default` that routes every other encoding back to a defined state.

Property: `<> (p5 || p6 || p7)` over the cube `{boot_fsm_ns ∈ {5,6,7}}` —
"the next-state register can transition into an undefined encoding." Under
`setundef -anyconst` (the power-up value is unconstrained):

```
pre_fix  (no default arm):    verdict cells T=7 F=1 ⊥=0   → hazard DEFINITELY present
post_fix (default + reset):   verdict cells T=4 F=1 ⊥=3   → hazard NO LONGER DEFINITE
```

**Reading the result honestly.** `pre_fix` is a sound CWE-1245 detection: every
cube is decided (`⊥=0`) and the undefined encoding is reachable (`T=7`). `post_fix`
is **`KleeneBot`, not "verified safe"**: the fix's `default: boot_fsm_ns =
boot_fsm_ps` *holds* the undefined encoding and the FSM only escapes via the reset
window, so the coarse `{p5,p6,p7}` cube **cannot prove the fixed FSM safe** — `⊥` is
the correct verdict. The milestone is the sound **pre/post distinction** (definite
hazard → indefinite), *not* a safety proof. Proving safety would need a finer
abstraction / CEGAR to convergence.

## API + UI

- **API:** `POST /api/v1/btor2/cegar` and `POST /api/v1/sv/cegar` ([`crates/mununu-core/src/api/handlers.rs`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs)) return the per-iteration trace + the `{T, F, ⊥}` verdict + the `warnings` list.
- **UI:** the `/cegar` panel renders the verdict with `KleeneBot` iconography, the per-iteration refinement trace, the final predicate set, and the soundness warnings; it round-trips a predicate edit and re-runs.

## See Also

- [RTL Verification Pipeline](RTL-Verification-Pipeline) — the complementary bit-blast engine
- [Mu-Calculus Reference](Mu-Calculus-Reference) — `<>` / `[]`, controllability operators, fixpoints
- [Adapter Formats](Adapter-Formats) — BTOR2 + SystemVerilog import
- [`docs/design/predicate-abstraction-recipe.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/predicate-abstraction-recipe.md) — the full operational recipe (predicate seeding, may/must image, CEGAR, §4.9 soundness)
- [`docs/design/kmts-theory.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/kmts-theory.md) — KMTS + 3-valued mu-calculus theory
- [`docs/design/post-rf5-architecture.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/post-rf5-architecture.md) — the whole picture: explicit vs symbolic engines, IR layering, may/must + over/under/⊥ approximation
