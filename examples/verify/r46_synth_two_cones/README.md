# R46-5 — synthetic "joint busts cap, clusters fit" regression

> **Synthetic fixture (hand-written, NOT vendored RTL).** Unlike the
> `m{0,1,2,3}_*` milestones — which verify real OpenTitan RTL — this
> fixture exists to regression-test the **per-cluster verification
> mechanism** (R.4.6) in isolation, with a design small enough to read but
> shaped to exercise the cap-overrun → per-cluster path exactly.

## What it demonstrates

`source/two_cones.sv` is two **independent** 11-bit counters that share
only `clk`/`rst`. Verified jointly the design has **22 state bits**, past
the explicit-state cap (`MAX_STATE_BITS = 20`). Each of the two
reachability properties reads only one counter, so their cones are
disjoint and cluster **K=2**.

`mununu verify` detects the cap overrun and falls back to **per-cluster
verification**: it partitions the properties by cone overlap, **slices**
the netlist down to each cluster's own 11-bit cone (removing the
out-of-cone counter, its next-state logic, and the inputs that only fed
it), and verifies each cluster separately. Both properties come back
`SATISFIED` over distinct cluster automata (`Circuit__cl0` /
`Circuit__cl1`).

```
clustered-COI: joint cone 5 signals, 2 cluster(s), max cluster cone 3 signals (reduces binding cone by 2 vs joint COI)
per-cluster verification: joint design exceeded the state-bit cap → 2 cluster(s) verified separately (2 property route(s))
a_max_reachable: SATISFIED (2048/2048 states, 1/1 initial) [inline, over = Circuit__cl0]
b_max_reachable: SATISFIED (2048/2048 states, 1/1 initial) [inline, over = Circuit__cl1]
```

## Why slicing, not pinning (soundness)

Per-cluster restriction is realised by **slicing** the BTOR2 to a
cluster's cone, not by pinning out-of-cone registers to a constant.
Pinning is a safety-only over-approximation, and because synchronous
transitions advance every register at once, it would drop the in-cone
counter's updates whenever the out-of-cone counter moved — making even
this reachability property report a spurious failure. Slicing removes the
out-of-cone state from the transition relation, so the sliced model is
bisimilar to the (hypothetical) joint model on the cluster's atoms and the
verdict is sound for the full mu-calculus, at every alternation depth.

## Run it

```bash
cargo build -p mununu-cli
./examples/verify/r46_synth_two_cones/validate.sh
```

Requires `yosys` (with `read_slang`) and `z3` on `PATH`
(`LIBRARY_PATH=/usr/local/opt/z3/lib` for z3). The BTOR2 is produced
inside mununu's own temp directory; nothing is written under `examples/`.

## Scope

This is the *narrow-cone* case (each cluster fits once isolated). The
*wide-field* case — a cluster carrying a register that busts the cap on
its own (a 32-bit timer, say) — additionally needs parameter
concretisation stacked on top of slicing; the industrial fixture that
exercises that combination (OpenTitan `sysrst_ctrl`) is tracked
separately.
