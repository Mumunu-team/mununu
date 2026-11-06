# R46-6 / GAP-2 — per-cluster slicing × param-concretization, composed

> **Synthetic fixture (hand-written, NOT vendored RTL).** This is the
> GAP-2 composition proof the R.4.6 de-risk called for: a design where the
> rescue requires BOTH per-cluster slicing AND param-concretization
> stacked together. It complements the two narrower R46 fixtures:
>
> | fixture | what it isolates |
> |---|---|
> | `r46_synth_two_cones` | per-cluster slicing only (narrow cones; "joint busts, clusters fit") |
> | `r46_gap2_wide_concretize` | param-concretization only (single wide cell; cap-fix + escape-to-OOB soundness) |
> | **this** | **both, composed** (wide cells *inside* each cluster's cone) |

## What it demonstrates

`source/three_wide.sv` is three **independent** 24-bit counters, each with
an output `aK_o = (cnt == 7)`. Each property reads one counter, so the
three cones are disjoint and cluster **K=3**.

Two primitives must stack for the rescue:

1. **Per-cluster slicing (R46-2/R46-3).** Joint width busts
   `MAX_STATE_BITS = 20`, so the bit-blaster overflows and falls back to
   per-cluster verification: it partitions the properties by cone overlap
   and slices the netlist down to each cluster's own counter. But slicing
   *alone* is not enough here — each sliced cone is still a **24-bit**
   counter (> 20), so a sliced cluster would itself overflow.

2. **Param-concretization (effective-bits cap accounting, GAP-2).** The
   sidecar declares each counter `bounded_counter bound=127`, concretizing
   it to the value set {0..127} = **7 effective bits**. Now each cluster's
   sliced-and-concretized cone is 7 bits ≤ 20 and fits.

The joint effective width is 3 × 7 = **21 > 20**, so per-cluster still
fires (the concretization does not, by itself, make the joint design fit);
each cluster is 7 bits and fits. The reachability targets (`cnt == 7`) are
in the concretized set {0..127} and genuinely reached, and each counter
escapes its set at 128 → an OOB sink (the soundness shape the realize
numericity-gate fix in PR #94 makes verify correctly).

## Expected result

```
reach_a7: SATISFIED  over Circuit__cl0
reach_b7: SATISFIED  over Circuit__cl1
reach_c7: SATISFIED  over Circuit__cl2
```

All three SATISFIED and non-vacuous, each routed to a distinct cluster
automaton. Without the sidecar the design is rejected at 72 raw state bits
— proving the concretization is load-bearing and slicing alone cannot
rescue a 24-bit-per-cluster cone.

## Run it

```bash
cargo build -p mununu-cli
examples/verify/r46_gap2_per_cluster_concretize/validate.sh
```

`validate.sh` requires `yosys` (with `read_slang`) on `PATH`. It asserts both
contract points: the raw design is rejected, and the
sliced-and-concretized clusters all verify SATISFIED over distinct
automata.

## Relationship to the industrial fixture

This is the synthetic stand-in for the GAP-2 shape real OpenTitan RTL
exhibits — e.g. `pattgen` channels (per-channel `data` 64b / `prediv` 32b
/ `clk_cnt` 32b counters) and `sysrst_ctrl` detection sub-blocks (32-bit
debounce/detect timers): independent clusters that each carry a wide field
needing concretization to fit. It proves the composition mechanism before
the heavier real-RTL vendoring.
