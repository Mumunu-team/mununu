# v8 — csrng assume-guarantee recoverability (exact-symbolic)

> **Source of truth:** [`adapter::btor2::symbolic_bitblast::exact_symbolic_verdict`](../../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs), the `@mununu_guarantee` carrier ([`scan_annotation_properties`](../../../crates/mununu-core/src/adapter/slang/verify_auto.rs)), and `--engine exact-symbolic` / `--pin` on `mununu sv verify-auto` — surface: CLI+API+UI.

An **assume-guarantee liveness** showcase on real OpenTitan RTL, decided by the
[exact-symbolic engine](../../../wiki/Exact-Symbolic-Engine.md). It demonstrates the
mununu-exclusive wedge: a **branching-time (`AG EF`) recoverability** verdict SVA
structurally cannot phrase, decided **exactly** (2-valued, never `⊥`), whose value
**flips on a single explicit environment assumption**.

## The property

Recoverability — "from every reachable state, can the FSM get back to `MainSmIdle`?":

```
always_recoverable = νY. ((μX. (idle ∨ ⟨⟩X)) ∧ [] Y)      -- AG EF idle
```

with `idle = (state_q == 55 = MainSmIdle)`. The inner `μX` is `EF idle` — *there
exists a path back to idle* — and the outer `νY` insists it holds at every reachable
state. The `EF` (existence of a continuation) is the obstruction for SVA and any
linear-time logic; in the mu-calculus it is one formula.

## The flip

The design's own SVA (`CsrngMainErrorStStable_A`) says `MainSmError` is a **stable
trap**: once there, the FSM stays there until reset. The only reachable path into it,
out of reset, is the input `local_escalate_i` (csrng_main_sm.sv:54-56 — a local
security escalation forces `state_d = MainSmError`). So recoverability depends
entirely on whether escalation is admitted:

| environment assumption | `AG EF idle` (exact-symbolic) |
|---|---|
| **none** — `local_escalate_i` free | **VIOLATED** (definite) — escalation latches the FSM in the `MainSmError` trap; idle unreachable |
| **`G ¬local_escalate_i`** — no escalation | **HOLDS** (definite) — the FSM always cycles back to idle |

The assumption is applied as an input concretization (`--pin
local_escalate_i=0`), which the exact engine bakes into the model before checking.
Both verdicts are 2-valued and definite — the exact engine uses no abstraction, so
there is no `⊥` and both answers transfer to the modeled design.

## Run it

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli   # once
examples/verify/v8_csrng_escalation_recoverability/validate.sh
```

Requires `sv2v` and `yosys` on `PATH` (and `z3`). The script annotates a temp copy of
`csrng_main_sm.sv` with the recoverability guarantee, runs `sv verify-auto --engine
exact-symbolic` twice (with and without the `local_escalate_i=0` assumption), and
checks the flip.

The same flip is a non-docker-optional regression at
[`e2e_h5gr1_csrng_recoverability_escalation_flip`](../../../crates/mununu-core/src/adapter/slang/verify_auto.rs)
(run in the `mununu-sva` image with `--ignored`).

## Honesty notes (claims-integrity)

- **Design-pattern demonstration, not a vulnerability finding.** The
  escalation-latching behaviour is the SEC_CM design intent — a local escalate is
  *supposed* to wedge the FSM in error until reset. The showcase demonstrates the
  *property class* on real silicon, not a bug.
- **Recoverability with reset available** is a companion question (the model here has
  reset gated inactive, so "recover" means "without a further reset"). The
  reset-availability variant is the sibling [v7](../v7_csrng_recoverability/) example,
  decided over the predicate-cube CEGAR path.
- **Exact means exact for the model.** The verdict is exact for the bit-blasted BTOR2
  yosys emits; its transfer to silicon still rests on the extraction (black-boxing,
  `setundef` discipline, the reset model) — the standard
  [claims-integrity](../../../docs/policies/claims-integrity.md) boundary.
