# R46-6 / GAP-2 — pattgen per-cluster × param-concretization (real OpenTitan RTL)

> **Real vendored RTL + a hand-written harness.** The channel logic is
> OpenTitan `pattgen_chan` (vendored, pinned in `source/UPSTREAM_COMMIT.txt`,
> Apache-2.0). The 2-channel harness and the sidecar are hand-written
> (clearly labeled NOT vendored). This is the industrial counterpart to the
> synthetic `r46_gap2_per_cluster_concretize` fixture: it shows the GAP-2
> composition on RTL whose channels really do carry wide datapath/counters.

## Why pattgen is a GAP-2 case

A single `pattgen_chan` is **169 state bits** — `data_q`[64] + `prediv_q`[32]
+ `clk_cnt_q`[32] + `reps_q`/`rep_cnt_q`[10] + `len_q`/`bit_cnt_q`[6] + a
handful of 1-bit FSM/config flops. One channel already busts
`MAX_STATE_BITS = 20` on its own. There is no "narrow real-RTL channel" to
use as a lighter rung — real channels carry wide fields. So the rescue
needs **both** R.4.6 primitives stacked:

1. **Per-cluster slicing** isolates each channel's cone (the two channels
   are independent → cluster K=2), but a sliced channel is still 169 bits.
2. **Param-concretization** (the sidecar) shrinks each channel's wide state
   cells to the small value sets this property actually needs.

## What the fixture verifies

`source/pattgen_harness.sv` instantiates two `pattgen_chan` with a small
constant configuration (prediv=1, a 1-bit pattern, 2 reps) and a free
per-channel enable. With that config each wide cell holds a tiny value set,
which `source/pattgen_harness.mununu.json` declares:

| cell | raw | concretized |
|---|---|---|
| `prediv_q`, `clk_cnt_q` | 32b | `bounded_counter bound=1` |
| `data_q` | 64b | `bounded_counter bound=1` |
| `reps_q`, `rep_cnt_q` | 10b | `bounded_counter bound=1` |
| `len_q`, `bit_cnt_q` | 6b | `ignored` (constant 0) |

Each channel then fits in ~14 effective bits; the joint design is ~28 > 20,
so per-cluster verification splits it into two 14-bit clusters. The two
reachability properties (`mu X. (doneK_o || <>X)`) come back **SATISFIED**,
each over a distinct cluster automaton (`Circuit__cl0` / `Circuit__cl1`),
on **128-state** clusters — i.e. the concretized wide cells, not a
degenerate sliced-away cone. Each channel's counters escape their
concretized sets → an OOB sink (the 129th state), the shape the realize
numericity-gate fix (#94) makes sound.

The reachability verdict is the same shape as the R46-5 mechanism fixture
(`r46_synth_two_cones`): this fixture exists to regression-test the
abstraction-composition **mechanism** on real RTL, not to make a deep
correctness claim about pattgen.

## Expected result

```
(1) no sidecar             → rejected at 338 raw state bits
(2) sidecar, one property  → rejected at 28 bits (joint busts; one cone, no split)
(3) sidecar, two properties→ per-cluster fires:
        reach_done0: SATISFIED 128/129 over Circuit__cl0
        reach_done1: SATISFIED 128/129 over Circuit__cl1
```

Both `(1)` and `(2)` are the load-bearing negatives: `(1)` shows
concretization is necessary, `(2)` shows per-cluster slicing is necessary
*on top of* concretization. Only `(3)` — both primitives composed — fits.

## Run it

```bash
cargo build -p mununu-cli
examples/verify/r46_pattgen_per_cluster/validate.sh
```

Requires `yosys` (with `read_slang`) on `PATH`.

## Provenance / claims integrity

- `pattgen_chan` and `pattgen_ctrl_pkg` are vendored verbatim from
  lowRISC/opentitan at the pinned commit; the channel logic is unmodified.
- The harness narrows the module's 116-bit `ctrl_i` struct (which bundles
  `enable` with the wide config and busts `MAX_INPUT_BITS`) to a small
  constant config + free enables. It does not change the channel's
  behaviour; it provides a verification interface and the concretization
  posture the sidecar then declares.
- The verdict is a model-level reachability check over the concretized
  abstraction — a mechanism demonstration, not an RTL bug finding.
