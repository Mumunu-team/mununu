# Consumer briefing — infeasible initial cubes no longer abstain (`$onehot` / enum properties over inputs)

> **Audience:** consumers of `mununu sv verify-auto` (CLI, `POST /api/sv/verify-auto`, mununu-ui)
> and anything that parses a `PropertyVerdict`. **ROSF, monono, mununu-ui.**

## TL;DR

A property whose atoms place **two or more mutually exclusive equalities on the same input
register** used to come back `Unknown` even when it plainly held. It now decides.

The canonical case is `$onehot0(oh_i)` / `$onehot(oh_i)` over a **primary input**, and any
enum-valued property (`sig == A`, `sig == B`, …) over one. Verdicts move
`Unknown { unknown_cells: N }` → `Holds` (or `Violated`). No verdict that was already definite
changes.

## What actually changed

`$onehot0(oh_i)` expands to one atom **per value** — `oh_i == 0`, `== 1`, `== 2`, `== 4`, `== 8`.
Over an input each becomes its own free cube dimension, so the reset state enumerated all
2⁵ = 32 polarity combinations. Only **6** of those are satisfiable (one per value, plus the
all-false case). The other **26 are contradictory** — no concrete state satisfies `oh_i == 1`
and `oh_i == 2` at once.

`downgrade_unsatisfiable_cells` already masked those 26 to ⊥ correctly. The defect was that they
were still enumerated as **initial** cubes, so the abstention census read an empty cube's ⊥ as a
genuine abstention: `Unknown { unknown_cells: 26 }`, and 26 = 32 − 6 exactly.

Initial-cube enumeration now drops valuations that assert one register at two different values.

### Soundness

This is an **exact reduction, not an approximation**. A cube asserting `reg == v₁ ∧ reg == v₂`
with `v₁ ≠ v₂` describes no state, so dropping it cannot lose a reachable initial state, and the
verdict it yields is the verdict over the same set of reachable states as before.

The symbolic path already took this view — `symbolic_final_verdict` projects infeasible cubes to
`F` as "never a reachable reset cube". This brings the explicit path into line with it; the two
paths previously disagreed.

## Scope — how to tell if you are affected

Unlike the internal-net change in `2026-09-internal-net-monitor-resolution.md`, this scope is
exactly characterisable. A property is affected **iff** its atoms put ≥ 2 mutually exclusive
equality dimensions on a single register that is a **primary input**.

- Over a **state** register the property already decided (one initial cube) — unaffected.
- With a **single** equality per register — unaffected.
- Properties that were already `Holds` / `Violated` — unaffected.

## Per-consumer

### ROSF / monono

- **What to update:** nothing required. Some properties reported as `unknown` now return a
  definite verdict.
- **What to expect:** `ci_exit_code` fails on `unknown` but never on `skipped`. If a suite was
  passing *because* a `$onehot`-over-input property abstained and something downstream tolerated
  it, that property now decides — and if it decides `Violated`, the gate will fail where it
  previously did not. **That is the gate working, not a regression**; the property was never
  checked before.
- **Report parsing:** no shape change. `unknown_cells` disappears from affected properties only
  because they no longer abstain.

### mununu-ui

- **What to update:** nothing. No wire-format change.
- **What to expect:** fewer ⊥ badges on one-hot / enum input properties.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust toolchain only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; slang/sv2v/yosys pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; MathSAT/pono pins unchanged | No |
| `hw-verif` | Not involved | No |

Consumers pick this up with a normal `cargo build` of the workspace.

## Test the transition

The change ships with a controlled contrast already in the suite:

- `e2e_opentitan_prim_onehot_check_holds` — `$onehot0` over an **input**. Was
  `Unknown { unknown_cells: 26 }`; now `Holds`.
- `e2e_onehot0_state_invariant_holds` — the same property over a **state** register, which always
  passed. It is the control: it confirms the filter did not disturb the path that already worked.

Both were measured in the `mununu-sva` image (a bare-host green on an SVA path is presumed
vacuous per CLAUDE.md):

```
running 2 tests
test adapter::slang::verify_auto::tests::e2e_onehot0_state_invariant_holds ... ok
test adapter::slang::verify_auto::tests::e2e_opentitan_prim_onehot_check_holds ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 2795 filtered out; finished in 13.16s
```

Unit-level, `infeasible_free_input_cubes_are_not_initial` pins 32 → 6, with
`distinct_registers_are_still_fully_enumerated` (8 stays 8) and
`same_register_same_value_is_not_excluded` (4 stays 4) as negative controls — a filter that
over-fires would drop those too.

## Provenance

- Issue: mununu#503 (e2e non-regression gate), surfaced by the #504 C5 gate.
- Fix: `free_input_init_cubes_feasible` in
  `crates/mununu-core/src/adapter/slang/verify_auto.rs`.
- Diagnosis: `.claude/plans/agile-munching-bear.md` § "Cause 1".
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- **`sva_12`** (`|=>` with a compound antecedent and a combinational consequent) is still ⊥. It
  is a different cause — the reducer for it does not exist yet — and is tracked separately.
- The feasibility check is **syntactic**: mutually exclusive equalities on one register. It does
  not detect semantic infeasibility arising across different registers (e.g. two dimensions made
  contradictory by an invariant). Those cubes are still enumerated, still masked to ⊥ by
  `downgrade_unsatisfiable_cells`, and can still abstain. Threading CEGAR's full `unsat_cells`
  set out of the trace is the complete fix and remains open.
