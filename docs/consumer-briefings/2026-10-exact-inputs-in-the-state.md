# Consumer briefing — 2026-10 exact engine: a property that reads a primary input is decided, not skipped (inputs in the state)

> **Audience:** anyone whose `sv verify-auto` run (exact engine, or the default portfolio whose first member it is) reports `skipped — exact MC: atom … is combinationally driven by primary input(s) …` or `… atom references primary input …`: monono's arbiter / handshake blocks, rosf lanes, and every SVA corpus where antecedents are gated by inputs (`valid && ready |-> …`).
>
> **Related:** [mununu#656](https://github.com/Mumunu-team/mununu/issues/656). Design note in the module doc of [`adapter/btor2/input_state.rs`](../../crates/mununu-core/src/adapter/btor2/input_state.rs). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change: `skipped` becomes a decided HOLDS / VIOLATED; one new CLI flag and API field; no wire-shape change.** The exact engine used to refuse any atom that reads a primary input, or a signal combinationally driven by one, because its states were register valuations and the input had no value *at a state*. It now evaluates such a property over the registers **with the input sampled in the same cycle** — the model SVA's own sampling semantics describes — so the atom is a state predicate and the property decides. Verdicts on properties that never read an input are unchanged.

## What changed

- [`input_state::lift_atom_inputs`](../../crates/mununu-core/src/adapter/btor2/input_state.rs): for each named primary input the property's atoms reach, a new state cell takes the input's symbol (free at cycle 0), the input line becomes the free next-cycle choice `name$next`, every reader is redirected to the cell, and `next cell = input`. Trace-equivalent on the registers; a same-cycle implication's antecedent and consequent read the *same* sampled value, a `|=>` consequent reads the next frame's.
- **Scope.** Only the inputs the atoms reach are lifted (the exact engine's bit cap moves by twice their width — the sampled cell and the next-cycle choice both enter the cone). Anonymous inputs — black-boxed submodule outputs, undriven nets — are still refused: a verdict over them would read a chaotic stub. The two-player verbs (`btor2 game`, synthesis) do not lift: a controllable input folded into the state is fixed before the controller moves.
- **`constraint` at frame 0 and at successors.** A BTOR2 `constraint` must hold at every frame of a valid trace. With an input in the state a constraint over it is a state predicate, so the engine now reads the verdict at the *legal* initial frames only (`init ∧ ∃i. c`) and its `<>` / `[]` steps land on legal frames only — the reading `exact_bad_reachable` already used for `bad`. A register-only design with a constraint that can have no admissible input at some reachable state may see a `VIOLATED` become `HOLDS` from this (the old verdict was read at a frame no valid trace visits).
- **Differential switch.** `--no-input-state` (CLI, `sv verify-auto` and `internal-engine-eval`), `no_input_state: true` (API), `MUNUNU_NO_INPUT_STATE=1` (process-global) restore the refusal. `docs/api-schemas/sv-verify-auto-request.schema.json` regenerated.

## Measured

| design · property | before | after |
|---|---|---|
| OpenTitan `prim_arbiter_tree` · `AG EF (gnt_o == 0)` (`gnt_o = req_i & sel(prio_mask_q) & ready_i`) | exact: skipped; cube (default `must_edge_inference = off`): ⊥, 4 cells | **HOLDS** (exact) |
| OpenTitan `prim_arbiter_ppc` · `AG EF (gnt_o == 0)` | skipped (after #657 vendored its submodule) | **HOLDS** (exact) |
| unit: one-register Mealy arbiter `gnt = req & ready & ~busy_q` — `AG EF (gnt == 0)` / `AG (gnt == 0)` / `AG (gnt → req)` / `AG (req → AX busy_q)` | all refused | HOLDS / VIOLATED / HOLDS / VIOLATED |
| unit: the same with `constraint !req` — `AG (req == 0)` | refused | HOLDS (vacuous — the constraint forbids `req` at every legal frame) |
| x629 fixture: `AG (addr == 4 → rdata == status_word)` with `constraint addr != 4`, `addr` an input | refused ("the antecedent still pins the input") | HOLDS (vacuous) |
| OpenTitan corpus census (19 properties, `differential_oracle_e2e`) | 16 decided, 3 ⊥ | **19 decided (11 HOLDS, 8 VIOLATED), 0 ⊥**; the strict monotone ledger passes |
| OpenTitan `sysrst_ctrl_detect`, its own 16 SVA (exact engine) | 7 VIOLATED — spurious, mununu#658 — + 9 HOLDS; the symbolic run skipped 12 | **16 HOLDS**; the explicit and symbolic(+exact-rescue) runs agree on every row |

Wall-class matrix: green (no lever contradicts an oracle verdict). Full e2e sweep (`scripts/e2e-sweep.sh`, one test per process): **46 passed, 0 failed, 0 crashed**.

**Also fixed on the way — mununu#658, pre-existing on `main`.** An atom over an internal `assign` wire (an Op symbol, e.g. `trigger_active = (trigger_i == 0)`) was never refused: the refusal's name map had no Op symbols, so such an atom was evaluated with its input quantified per step — a spurious VIOLATED on seven of `sysrst_ctrl_detect`'s own assertions, masked whenever a bare input atom sat beside it. Both the refusal and the lift now resolve Op symbols.

## What to update, per consumer

- **monono / rosf:** properties pinned by name as `skipped` on either refusal text will now decide. Unpin them and read the new verdicts; a new `VIOLATED` is a real refutation over the sampled-input model (the witness shows the input per step). If a lane relies on the refusal as a guard, pass `--no-input-state`.
- **Report parsers:** nothing changed in the shape. The request schema gained the optional `no_input_state` field.
- **Everyone:** a property that reads a wide input bus pays for it in BDD variables. If a previously-fast run now abstains on the bit cap, the input width is the cause; `--no-input-state` restores the old skip.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the exact engine's model for input-reading properties | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x656_                                      # the rewrite, the engine on the product, the frame-0 filter
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --test differential_oracle_e2e --all-features -- --ignored --nocapture diff_corpus_verdict_census
```

## Not covered here

- The cube engine still returns ⊥ on `AG EF` over an input-driven atom under verify-auto's default `must_edge_inference = off` (an `EF` needs must-edges); `--must-edge-inference smt-per-target-standard` decides it, and `smt-hyper-must` does not pin a combinational target dimension — tracked separately.
- The two-player path keeps the refusal; lifting only the *uncontrollable* inputs there is a follow-up with its own soundness argument.
- The env-strategy (assumption discovery) path reads the verdict at every initial state, not only the legal ones; it does not use the lift.
