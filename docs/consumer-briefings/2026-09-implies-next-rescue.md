# Consumer briefing — `A |=> C` properties now decide via the safety-rescue lane

> **Audience:** consumers of `mununu sv verify-auto` (CLI, `POST /api/sv/verify-auto`, mununu-ui)
> and anything that parses a `PropertyVerdict`. **ROSF, monono, mununu-ui.**

## TL;DR

SVA's non-overlapped implication `A |=> C` — "if `A` holds this cycle, `C` holds the next" — used
to come back `Unknown` whenever the predicate cube could not pin it. It now reduces to a `bad`
monitor and is decided by the reachability portfolio.

Measured on OpenTitan's `sysrst_ctrl_detect` (16 assertions, `mununu-sva` image): **HOLDS 12 → 14,
UNKNOWN 4 → 2** under config concretization. Two properties moved, both `⊥ → Holds`. No verdict
that was already definite changed, and nothing became `Violated`.

## What actually changed

`A |=> C` lifts to `nu X. ((¬A ∨ [] C) ∧ [] X)`, i.e. `AG (A → AX C)`. The safety-rescue lane had
no reducer for that shape, so it declined with `bottom-reason: safety-shape-not-reducible`.

An emitter for the shape did exist but could not reach a real property: it took a **single atom**
as the antecedent and required the consequent to be a **state cell**. Real assertions are not
shaped that way — OpenTitan writes

```systemverilog
`ASSERT(DetectStDropOut_A,
    state_q == DetectSt && !trigger_active && cfg_enable_i
    |=>
    state_q == IdleSt)
```

a three-term antecedent, and `sysrst_ctrl_detect_sva_12`'s consequent
(`!event_detected_pulse_o`) is combinational, not a state cell. Both limits are lifted: the
antecedent may be any boolean tree, and the consequent may be combinational.

The monitor synthesises a 1-bit latch (`mununu_ante_prev`, `init 0`, `next = A`) and asserts
`bad = mununu_ante_prev ∧ ¬C`.

### Soundness

`bad` is reachable ⟺ ∃ a trace and a cycle `t` with `A` true at `t` and `C` false at `t+1` —
exactly a violation of `AG (A → AX C)`, and exactly SVA's own reading of `A |=> C`. Three things
carry that equivalence:

- **`init 0` fabricates no obligation at reset.** Nothing precedes cycle 0. An `init 1` latch would
  report a violation on a design that never asserted `A`; a regression test pins this.
- **The latch is a pure monitor.** It adds no input and never feeds back into the transition
  relation, so the reachable-state set is unchanged and a counterexample projects back onto the
  original registers.
- **A free next-cycle input is the correct reading of `[] C`.** A combinational `C` depends on the
  inputs at `t+1`; `bad` reachability quantifies existentially over those, which is precisely
  `¬(∀ successors. C)`.

## Scope — how to tell if you are affected

A property is affected **iff** it is an `A |=> C` (non-overlapped implication) that was previously
returning `Unknown`. Overlapped `A |-> C`, plain invariants, and anything already definite are
untouched.

**What still declines**, and will keep reporting `safety-shape-not-reducible`:

- **Register-vs-register consequents.** `sva_13`'s `cnt_q >= cnt_q__past` and `sva_15`'s
  `cnt_q == cnt_q__past + 1` are `CmpReg` / `CmpRegAddend` leaves, which the boolean leaf compiler
  does not accept. These are the 2 residual ⊥ above.
- **Nested-box consequents.** `a |=> ##1 b`, and the nested form the translator emits for
  multi-element antecedents (`a ##1 b |=> c`).

Both decline rather than being mis-compiled. A modality silently flattened into a boolean would be
a soundness bug, not a wider reach.

## Per-consumer

### ROSF / monono

- **What to update:** nothing required.
- **What to expect:** `|=>` properties that abstained now return a definite verdict. `ci_exit_code`
  fails on `unknown` but never on `skipped`, so a suite that was passing *because* a `|=>` property
  abstained will now see it decide. On this corpus every newly-decided property was `Holds`; a
  `Violated` is possible in general, and would mean the gate is catching something it previously
  could not check.
- **Report parsing:** no shape change. Newly-decided properties carry a `portfolio-rescue`
  verification note naming the deciding engines.

### mununu-ui

- **What to update:** nothing. No wire-format change.
- **What to expect:** fewer ⊥ badges on `|=>` assertions; a `portfolio-rescue` note where one
  appears.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; slang/sv2v/yosys pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; MathSAT/pono pins unchanged | No |
| `hw-verif` | Not involved | No |

## Test the transition

Measured in the `mununu-sva` image (a bare-host green on an SVA path is presumed vacuous per
CLAUDE.md):

```
note [portfolio-rescue] `sysrst_ctrl_detect_sva_12`: the cube abstraction left ⊥;
     the reachability portfolio decided it HOLDS (engine(s): exact, native)
note [portfolio-rescue] `sysrst_ctrl_detect_sva_14`: the cube abstraction left ⊥;
     the reachability portfolio decided it HOLDS (engine(s): exact, native, spacer)
note [coverage-summary] 16 assertion(s): 14 definite (HOLDS), 0 violated, 2 unknown (⊥)
```

- `sva_12` = `event_detected_pulse_o |=> !event_detected_pulse_o` — combinational consequent.
- `sva_14` = `cnt_clr |=> cnt_q == 0` — plain comparison consequent.

**On cross-checking these.** `concrete_oracle::ag_implies_next` cannot serve as the differential
here: it evaluates the consequent as a function of the next *state* and refuses an input-dependent
one. The exact-symbolic μ-calculus engine also abstained on this design (BDD arena exhausted / the
derived-predicate limitation). What the verdicts do carry is **multi-engine agreement inside the
reachability portfolio** — `exact` (BDD reachability, a proof of unreachability) alongside `native`
BMC, and `spacer` additionally on `sva_14`. That is the honest strength of the evidence: a
BDD-reachability proof plus concurring engines, not an independent μ-calculus cross-check.

Unit-level, the monitor's two load-bearing properties are mutation-tested: an `init 1` latch fails
`implies_next_makes_no_obligation_at_reset`, and a latch that never asserts fails
`implies_next_catches_a_pulse_that_can_stay_high` and
`implies_next_accepts_a_compound_antecedent`. Both mutants were run and confirmed to fail.

## Provenance

- Issue: mununu#503 (e2e non-regression gate), surfaced by the #504 C5 gate.
- Fix: `reach_rescue::reduce_ag_implies_next` +
  `btor2::bad_monitor::emit_ag_implies_next_compound_monitor`.
- Diagnosis: `.claude/plans/agile-munching-bear.md` § "Cause 3".
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The register-vs-register leaf gap (`CmpReg` / `CmpRegAddend`) — the 2 residual ⊥ on this corpus.
  Widening the leaf compiler to relational leaves is a separate change.
- The four e2e failures this closes out had **four different causes**; the other three are covered
  by `2026-09-internal-net-monitor-resolution.md` and `2026-09-infeasible-initial-cubes.md`.
