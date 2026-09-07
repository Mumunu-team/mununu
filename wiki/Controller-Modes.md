# Controller Modes

mununu's `synth` subcommand offers six **controller extraction modes**. They differ in (a) how aggressively they prune the winning region into a deterministic strategy, and (b) how much memory the resulting controller carries.

Pick a mode based on what you want from the controller: a maximally permissive supervisor, a deterministic Mealy machine, or a memory-aware strategy that handles complex liveness properties.

| Mode | Default | Determinism | Memory | Best for |
|---|---|---|---|---|
| `projection` | ✅ default | non-deterministic (keeps all winning transitions) | none | Inspecting the winning region |
| `functional` | | deterministic (one per state) | none | Standard Mealy controller for safety / single-mu reachability |
| `permissive` | | non-deterministic | none | Composable Ramadge-Wonham supervisor |
| `signature-memory` | | deterministic | none (annotation only) | Like `functional` but state names expose obligation rank |
| `product-game` | | deterministic | mu-obligation index | GR(1) / Buchi (alternation 2) — fair across multiple mu-obligations |
| `parity-game` | | deterministic | full formula sub-node | Arbitrary alternation depth (parity properties) |

## Mode reference

### Projection (default)

Keeps every transition between winning states. The controller is **not a strategy** — it's the winning region as a sub-CLTS. Useful for inspection but not deployable as a deterministic Mealy machine.

```bash
mununu context synth model.ctxdsl --formula safety --automaton M
# (no flag, or --controller-mode projection)
```

### Functional (`--controller-mode functional`)

Deterministic strategy: at each winning state, picks **one** controllable transition — the one whose target has the lexicographically smallest signature (best mu-progress). All uncontrollable transitions are kept. Correct for any mu-calculus formula's winning verdict; for liveness properties at alternation ≥ 2, see `product-game` and `parity-game` below.

This is the legacy `--extract-strategy` flag's behavior.

### Permissive (`--controller-mode permissive`)

Maximally permissive supervisor (Ramadge-Wonham canonical object). Keeps all controllable transitions whose target signature is ≤ the source's signature. Non-deterministic but composable with other supervisors.

### Signature memory (`--controller-mode signature-memory`)

Same selection rule as `functional`, but state names are annotated with the iteration-rank signature: `<state>__sig_<rank0>_<rank1>_...`. The controller is functionally identical to `functional`, but the obligation rank is now an observable part of the controller's state — useful for cascading supervisors and downstream tools.

### Product game (`--controller-mode product-game`)

Memory-aware Mealy controller for **alternation ≥ 2** formulas (GR(1), Buchi-style `GFp ∧ GFq`). State space is `(plant_state, oblig_idx)` where `oblig_idx` rotates round-robin through the formula's mu-fixpoints to ensure fairness — at each product state the controller drives progress on the active obligation; when locally satisfied, memory advances to the next obligation. State names: `<state>__pg_<i>`.

Reference: Bruse, Friedmann & Lange, **SPIN 2016**.

### Parity game (`--controller-mode parity-game`)

Full parity-game synthesis. Builds the explicit parity game over `(plant_state, formula_subnode)` positions, solves via Zielonka's recursive algorithm, and projects Eve's positional strategy to a Mealy controller. **Correct for arbitrary alternation depth** (parity properties of any depth) — strictly more general than `product-game` at the cost of a larger state space (one product state per pair of plant state and formula sub-node). State names: `<state>__pg_n<node_id>`.

Reference: Zielonka 1998 (positional determinacy of parity games), Emerson-Jutla.

## When to use which

```
formula has alternation ≥ 3?
├─ yes → parity-game
└─ no
   ├─ alternation = 2 (GR(1), Buchi)?
   │  ├─ yes → product-game (cheaper) or parity-game (uniform)
   │  └─ no
   │     ├─ alternation 0 or 1 (safety / reachability)?
   │     │  ├─ yes → functional (deterministic) or permissive (composable)
   │     │  └─ no
   │     │     └─ projection
```

If you don't know the alternation depth, `parity-game` always works but produces the largest controller. `functional` is a safe default for anything that's not a fairness/liveness property.

## CLI usage

```bash
# Default (projection)
mununu context synth elevator.ctxdsl --formula door_always_closes --automaton Elevator

# Memory-aware Mealy
mununu context synth elevator.ctxdsl --formula door_always_closes --automaton Elevator \
    --controller-mode product-game

# Full parity-game synthesis
mununu context synth elevator.ctxdsl --formula door_always_closes --automaton Elevator \
    --controller-mode parity-game
```

The legacy `--extract-strategy` flag still works (equivalent to `--controller-mode functional`). When both `--extract-strategy` and `--controller-mode` are supplied, `--controller-mode` wins.

## API usage

`POST /api/v1/context/synthesize`:

```json
{
  "context": { "name": "elevator.ctxdsl", "content": "context test {...}" },
  "automaton": "Elevator",
  "formula": "door_always_closes",
  "options": {
    "minimize": true,
    "controller_mode": "parity-game"
  }
}
```

`controller_mode` accepts the same names as the CLI, case-insensitive (dashes/underscores interchangeable). When omitted and `extract_strategy=true`, falls back to `functional`. Otherwise defaults to `projection`. Unknown values return `400 Bad Request` with a list of valid names.

## UI usage

The synthesis options panel in [mununu-ui](https://github.com/your-org/mununu-ui) exposes a **Extraction mode** dropdown above the controllers table. The selection applies to all subsequent controller exports until changed.

## Example: alternation-4 benchmark

The shipped example [`examples/bus_arbiter_retry.ctxdsl`](https://github.com/Mumunu-team/mununu/blob/main/examples/bus_arbiter_retry.ctxdsl) is a 4-state bus-arbiter system whose property has alternation depth **4** — two strict mu/nu alternations beyond GR(1). It demonstrates `parity-game` mode running on a formula that's strictly outside the `product-game` mode's correctness guarantee.

The property is the **recurrence-after-stability** pattern from the Manna-Pnueli temporal hierarchy (Σ₃-complete; appears in SYNTCOMP `lily-demo` and `amba_decomposed_lock_*`):

```
G(req → F(ack ∧ G(retry → F done)))
```

In plain English: every request must eventually be acknowledged, and *from that moment on*, every retry must eventually be answered by `done` — forever after the first ack. The "forever after" obligation is conditional on having entered the granted region, which the adversary may delay arbitrarily; this resists collapse to GR(1).

In mu-calculus this becomes `νZ. μY. νX. μW. (...)` — three strict mu/nu alternations.

Run it:

```bash
mununu context synth examples/bus_arbiter_retry.ctxdsl \
    --formula recurrence_after_stability --automaton BusArbiter \
    --controller-mode parity-game
```

State counts across the modes (representative output):

| Mode | Controller states |
|---|---|
| projection / functional / permissive / signature-memory | 4 (= plant size) |
| product-game | 8 (= plant × 2 mu-obligations) |
| parity-game | 78 (= plant × formula sub-nodes in winning region) |

All modes happen to agree on realizability for this small system because it admits a positional strategy. Parity-game's larger state space reflects the explicit product game over `(plant_state, formula_subnode)` pairs — the canonical encoding for arbitrary alternation. Lower modes emit a `[SOUNDNESS WARNING] Trust level: LOW` for alt ≥ 2 formulas, signaling that their strategy isn't proven correct in general at that depth.

## Example: dual-channel arbiter — memory + strategy selection

[`examples/dual_arbiter_alt4.ctxdsl`](https://github.com/Mumunu-team/mununu/blob/main/examples/dual_arbiter_alt4.ctxdsl) extends the bus arbiter with a SECOND independent channel. The property is the conjunction of two recurrence-after-stability obligations (one per channel) — still alternation 4. This example exercises both required behaviours of memory-aware synthesis:

1. **The controller must remember which liveness goal it is currently servicing.** Both channels can be in `Pending`/`Granted`/`Retrying` simultaneously, and the controller must rotate between obligations to make fair progress on both.
2. **The controller must disable controllable transitions.** Each `Granted` state has multiple outgoing controllables (`release` + `swap`); the synthesized strategy picks one and implicitly disables the others.

Mode-by-mode results on the 16-state composed system:

| Mode | Controller states | Transitions | Notes |
|---|---|---|---|
| `projection` | 16 | 48 | Keeps every winning controllable — not a strategy |
| `functional` | 16 | **31** | Picks one controllable per state — disables 17 transitions |
| `product-game` | 64 | 128 | Plant × 4 mu-obligations (YA, WA, YB, WB) |
| `parity-game` | 585 | 729 | Full game graph, parity-correct strategy |

Run it:

```bash
mununu context synth examples/dual_arbiter_alt4.ctxdsl \
    --formula dual_recurrence_after_stability --automaton System \
    --controller-mode parity-game
```

The 17-transition gap between `projection` and `functional` is the most direct evidence of strategy selection — those 17 controllables are the ones the synthesis explicitly omits.

## Implementation references

- [`crates/mununu-core/src/context/mod.rs`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/context/mod.rs) — `ControllerMode` enum + synthesis branches
- [`crates/mununu-core/src/mu_calculus/parity_game.rs`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/mu_calculus/parity_game.rs) — explicit parity-game construction + Zielonka solver
- [`crates/mununu-core/tests/soundness.rs`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/tests/soundness.rs) — per-mode regression + cross-mode realizability agreement tests
