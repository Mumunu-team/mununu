# Consumer briefing — 2026-10 exact two-player verbs: an atom over an environment input is decided (inputs in the state, environment side only)

> **Audience:** anyone running the game verbs (`btor2 game`, `btor2 synth`, the realizability / strategy endpoints, the two-player rows of a verify lane) on RTL whose goal or guarantee atom reads a primary input or a signal combinationally driven by one.
>
> **Related:** closes [mununu#661](https://github.com/Mumunu-team/mununu/issues/661) (follow-up to [#659](https://github.com/Mumunu-team/mununu/pull/659) / briefing [`2026-10-exact-inputs-in-the-state.md`](2026-10-exact-inputs-in-the-state.md)). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change on the game verbs (a refusal becomes a verdict); no wire-shape change; no new flag.** The inputs-in-the-state rewrite now applies on the two-player path for the **uncontrollable** inputs an atom reads: the input becomes a sampled state cell, its next-frame value the free `name$next`, so `o = q | req_i` is a state predicate and the game decides. An atom reaching a **controllable** input stays refused — by name, with the reason — because folded into the state it would be fixed before the controller moves. The lifted next-frame inputs are quantified **after** the controller's move (`∀env ∃ctrl ∀env_next`): the controller gains no look-ahead, so a verdict is the Mealy game's verdict.

## What changed

- [`lift_atom_inputs_excluding`](../../crates/mununu-core/src/adapter/btor2/input_state.rs): the one-player lift minus an exclusion set; `exact_symbolic_verdict_with_witness_inner` passes the controllable names, so only environment inputs are lifted on the game verbs.
- [`ExactModel::env_next_cube`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs): a third input cube for the lifted `name$next` bits. `cpre_controllable` = `∀env ∃ctrl ∀env_next ∃s′`, `cpre_environment` = `∃env ∀ctrl ∃env_next`, and the forcing-move extractions (`ctrl_forcing_moves` / `env_forcing_moves`) use the same order, so a synthesized Mealy strategy is a function of `(state, env)` only. Without a lift the cube is `tt` and nothing changes.
- The refusal for an atom over a controllable input now ends: "A controllable input stays a per-step choice — folded into the state it would be fixed before the controller moves, which changes who wins — so an atom over it is refused on the game verbs."
- Opt-out: `MUNUNU_NO_INPUT_STATE=1` restores the refusal on the game verbs too (they take no per-request exact-engine options today, so there is no `--no-input-state` on `btor2 game` — a follow-up if a lane needs the differential switch per request; `sv verify-auto` keeps its flag / `no_input_state` field).

## Why the quantifier order is the whole change

In the Mealy game the environment chooses `req_k` *after* the controller's `grant_{k-1}` fixed `q_k`. After the lift, `req_k` is chosen one round earlier as `req$next_{k-1}` — in the same round as `grant_{k-1}`. Left in the outer `∀env`, the controller would pick `grant_{k-1}` knowing `req_k`: `EF (q == req)` (match the environment's same-frame input) would become realizable. Quantified inside the controller's existential, the controller commits first and the environment answers — the original information structure; at frame k the controller sees `req_k` (a state cell now) when choosing `grant_k`, exactly as a Mealy controller does. The unit gate `x661_a_lifted_environment_input_gives_the_controller_no_look_ahead` pins `EF (q == req)` unrealizable.

## Measured

| design · goal | before | after |
|---|---|---|
| one-register arbiter (`q′ = grant`, `req` environment), `EF (o == 1)` with `o = q ∨ req` | refused: "atom `o` is combinationally driven by primary input(s) `req`" | **realizable** |
| same, `EF (a == 1)` with `a = q ∧ req` | refused | **unrealizable** (the environment starves it) |
| same, `EF (m == 1)` with `m = (q == req)` — the look-ahead trap | refused | **unrealizable** (a naive lift would say realizable) |
| same, `EF (g == 1)` with `g = grant` (controllable) | refused | refused, naming the controllable input |
| `wall_class_matrix` two-player rows | TODO | TODO |
| `btor2 game` e2e set (mununu-sva image) | TODO | TODO |

## What to update, per consumer

- **Lanes running game verbs on RTL with input-driven goal atoms:** a property that used to come back as a refusal / `Skipped` now decides; a lane that pinned the skip by name should expect a verdict. Realizable verdicts are the Mealy game's (no look-ahead); a strategy table still maps `(state, env inputs)` to controllable moves — the lifted inputs appear as state cells, not as environment columns.
- **Lanes that declare a controllable input the goal atom reads:** still refused, with the reason; restructure the goal over a register the controllable input drives (`q′ = grant`) rather than the input itself.
- **Report parsers:** no shape change.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the game verbs' verdicts | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x661_
mununu btor2 game <design>.btor2 --good "o == 1" --controllable grant                         # decides
MUNUNU_NO_INPUT_STATE=1 mununu btor2 game <design>.btor2 --good "o == 1" --controllable grant   # the old refusal
```

## Not covered here

- The symbolic (predicate-cube) two-player engine is untouched; the lift is an exact-engine rewrite.
- Anonymous inputs (black-boxed submodule outputs) stay refused on both paths.
