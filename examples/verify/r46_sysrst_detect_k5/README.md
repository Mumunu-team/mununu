# R46-6 / GAP-2 at K=5 — sysrst_ctrl detect per-cluster × param-concretization

> **Real vendored RTL + a hand-written harness.** The detector logic is
> OpenTitan `sysrst_ctrl_detect` (vendored, pinned in
> `source/UPSTREAM_COMMIT.txt`, Apache-2.0). The K=5 harness and the
> sidecar are hand-written (clearly labeled NOT vendored). This is the
> K=5 industrial scale-up of the pattgen (K=2) fixture: the headline
> sysrst_ctrl clustering case the R.4.6 plan named.

## Why sysrst_ctrl is the K=5 driver

OpenTitan `sysrst_ctrl` runs **K=5 independent debounce/detect sub-blocks**
(autoblock / ulp / keyintr / combo / pin), each an instance of the
parameterized `sysrst_ctrl_detect` timer block, with disjoint CSR groups
that share only a small synced input bundle. The joint cone busts
`MAX_STATE_BITS = 20` through the **five 32-bit detect/debounce timers**,
while each detector cluster fits once its timer is param-concretized — the
exact GAP-2 shape, at K=5.

This fixture reproduces that shape directly: `source/sysrst_harness.sv`
instantiates five `sysrst_ctrl_detect` blocks (five distinct EventTypes —
level, edge, sticky — mirroring the real top's mix) with a free per-detector
trigger + enable and small constant timer thresholds. It bypasses the
TileLink-UL register file (irrelevant to the per-cluster mechanism).

## What the fixture verifies

Each `sysrst_ctrl_detect` carries a 32-bit timer counter `cnt_q` plus a
2-bit FSM (`state_q`) and, for edge detectors, a 1-bit edge flop. The
sidecar concretizes each timer:

| cell | raw | concretized |
|---|---|---|
| `u_detK.cnt_q` (×5) | 32b | `bounded_counter bound=7` |

State-bit budget:

```
no sidecar:        5 × (32-bit timer + FSM + flops)  = 172 bits → rejected
sidecar, 1 prop:   concretized joint                 =  27 bits → rejected (one cone, no split)
sidecar, 5 props:  per-cluster → each detector        ≈ 5–6 bits → fits
```

The five reachability properties (`mu X. (detK_o || <>X)`) come back
**SATISFIED**, each over a distinct cluster automaton
(`Circuit__cl0 … cl4`), on **32–64 state** clusters (the concretized timer
+ FSM, not a degenerate sliced-away cone). Each timer escapes its
concretized set at the threshold → an OOB sink, the shape the realize
numericity-gate fix (#94) makes sound.

The reachability verdict is the same shape as the R46-5 mechanism fixture:
this fixture exists to regression-test the abstraction-composition
**mechanism** on real RTL at K=5, not to make a correctness claim about
sysrst_ctrl.

## Expected result

```
(1) no sidecar              → rejected at 172 raw state bits
(2) sidecar, one property   → rejected at 27 bits (joint busts; one cone, no split)
(3) sidecar, five properties→ per-cluster fires:
        reach_det0: SATISFIED over Circuit__cl0   (level)
        reach_det1: SATISFIED over Circuit__cl1   (level)
        reach_det2: SATISFIED over Circuit__cl2   (edge)
        reach_det3: SATISFIED over Circuit__cl3   (edge)
        reach_det4: SATISFIED over Circuit__cl4   (level, sticky)
```

`(1)` and `(2)` are the load-bearing negatives: `(1)` shows concretization
is necessary, `(2)` shows per-cluster slicing is necessary *on top of* it.

## Run it

```bash
cargo build -p mununu-cli
examples/verify/r46_sysrst_detect_k5/validate.sh
```

Requires `yosys` (≥ 0.67, with `read_slang`) on `PATH`. `sysrst_ctrl_detect` does
`` `include "prim_assert.sv" ``; the stub resolves on the verify path
because the lift's staging tempdir is on the include search path (added in
the same PR as this fixture).

## Provenance / claims integrity

- `sysrst_ctrl_detect` + `sysrst_ctrl_pkg` are vendored verbatim, pinned;
  the detector logic is unmodified.
- `prim_assert.sv` is a synthesis-equivalent empty-SVA-macro stub (the same
  one the M.2 fixture uses).
- The harness gives each detector a narrow free trigger/enable + small
  constant timer thresholds; it does not change the detector's behaviour.
- The verdict is a model-level reachability check over the concretized
  abstraction — a K=5 mechanism demonstration, not an RTL bug finding.
