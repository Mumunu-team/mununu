# BDD variable ordering in the exact-symbolic engine

> Source of truth: [`BddBitBlaster::build`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) (`MUNUNU_BDD_VAR_ORDER`, `MUNUNU_BDD_INPUTS_LAST`, `MUNUNU_BDD_ORDER_DEBUG`) — surface: CLI-only — an engine tuning knob consumed by CI lanes and diagnostics, not by a CTXDSL author.

**engine:** `exact-symbolic` (full-state ROBDD, OxiDD). Structure: full-state ROBDD over register+input bits. Technique: bit-blast μ-fixpoint with functional next-state substitution. Role: decider.

Measured 2026-09-14 while triaging [mununu#553](https://github.com/Mumunu-team/mununu/issues/553). **The default is unchanged (`cell-major`); everything here is opt-in.**

## The orders

```
cell-major (default)  a0 a1 a2 … b0 b1 b2 … c0 c1 c2 …
interleaved           a0 b0 c0   a1 b1 c1   a2 b2 c2 …
deps                  interleaved WITHIN a cluster of cells some expression reads together;
                      clusters laid out contiguously
```

## Why order matters here, and what does NOT apply

A BDD tests variables in a fixed root-to-leaf order. To evaluate `a == b` under cell-major the
diagram must remember **all** of `a` before it sees any bit of `b` — a 2^w frontier. Interleaved
compares bit by bit and carries only "equal so far": O(w) nodes.

**The classic interleave-x-with-x′ rule does not apply to this engine.** We compute the successor
by *functional substitution* — [`ExactModel::to_next`] substitutes each state variable with its
next-state function — not relationally over primed variables. `total_bits` counts register+input
bits only; there are no primed variables to place. What governs cost instead is **dependency
locality**: a variable should sit near the variables its own next-state function reads.

## Measurements

Verdicts were **identical in every arm of every run** below — ROBDDs are canonical, so order
changes size and never the answer. A moved verdict would be a bug, not a trade-off.

### Synthetic shapes

| shape | cell-major | interleaved |
|---|---|---|
| `a == b`, n=12 / 16 / 18 | 24,605 / 393,257 / 1,572,911 | 294 / 488 / 603 (**84× / 806× / 2,608×**) |
| `x >> k`, symbolic `k`, 12-bit | 65,537 | **1** |
| `x >> k`, symbolic `k`, 16-bit | 4.22 s | **0.16 s** |
| `x >> 5`, constant amount | 1 | 1 — it is wiring |
| `a * b == 1` (**control**) n=8/10/12 | 27,459 / 191,394 / 1,268,011 | 1.06× / 1.01× / **0.97×** |

The arithmetic row is the control and it must **not** improve: multiplication has exponential BDD
size under every variable order (Bryant 1986). It does not — which is what licenses reading the
other rows as an ordering effect rather than a harness artefact.

### Whole `mununu-core` lib suite (2,619 tests, 0 failures in every arm)

| order | time |
|---|---|
| cell-major | 123.8 s |
| **interleaved** | **48.2 s** |
| deps | 113.5 s |

The entire gap is **one test**: `relational_recoverability_target_decides_at_scale`, at
cell-major 195.0 s vs interleaved **1.44 s** (135×).

### A consumer's four widest RTL blocks

| block | default | interleaved | |
|---|---|---|---|
| `tlm_tx` (22 barrel shifts, 32-bit) | 20,185,089 | **1** | 20.2 M× |
| `sprite_render` | 35,505,958 | 14,444 | 2,458× |
| `affine_sampler` | 47,140,110 | 409,687 | 115× |
| `sdram_burst` (52 barrel shifts, 33-bit) | 60,801,327 | 56,452,475 | **9.4× SLOWER in time** |

## Findings

**1. Variable-amount shifts are the dominant cost, and they are purely an ordering artefact.**
A shift by a *symbolic* amount is a barrel shifter — every output bit becomes a mux tree over
every input bit selected by the amount bits — so the amount register must be interleaved with the
register it shifts. Cell-major grows exponentially in width here; interleaved is flat at one node.
A *constant* shift amount costs nothing under either order.

**2. The `part-select at a computed index` idiom is the source.** `drops_q[int'(sel)*W +: W]`
lifts to a constant `mul` (free — the partials collapse to a plain shift) **plus a symbolic
shift** (catastrophic under cell-major). A synthesiser emits a mux for the whole idiom. CLAUDE.md
*mandates* flat-vector-plus-part-select because it is what lifts at all, so the convention is what
puts barrel shifters in the cones — and our order is what made them expensive.

**3. `deps` does not pay off, for a structural reason.** On every real lift measured it produces
**one cluster containing all cells** (uart 10/10, spiCtrl 7/7, sd_data_master 10/10) — transitive
closure merges everything — so it degenerates to full interleaving. And it cannot see the coupling
that matters for formula-driven verbs, because `BddBitBlaster::build` fixes the order **before any
formula is known**. Seeding hyperedges from `bad`/`constraint`/outputs closed only ~8% of the
suite gap. Closing it properly means passing the property's atoms into the ordering — an
architectural change, not a constant. `MUNUNU_BDD_ORDER_DEBUG=1` prints the cluster structure.

**4. `inputs-last` is design-dependent in both directions, and conflicts with `deps`.**
`diamond_pre` does `∃ inputs` and quantification is cheapest near the leaves, so inputs want to be
last. But a symbolic shift amount has no `next` line, so it is classified input-like — and moving
it destroys the barrel-shifter coupling (1 → 65,537). The shipped version therefore never breaks a
coupled group, which costs a measured 4× on `uart_msg_handler`. Neither composition dominates.

**5. Ordering cannot help an ITERATION-bound cone.** The fixpoint depth is the reachability
diameter, an invariant of the transition system — see `measure_bdd_actual_size`. A 640×480 raster
wrap needs 818,626 iterations under any order.

## Why the default has not changed — the counter-example SURVIVED its test

One block regresses, and the settling measurement has now been made: same block, **same arena
(67 M fixed)**, same binary, 13 definite verdicts in all three arms.

| `sdram_burst` | time | |
|---|---|---|
| cell-major (default) | 48 s | |
| interleaved | 525 s | **10.9× slower** |
| `deps` | 461 s | **9.6× slower** |

So the earlier 52 s → 488 s was **not** arena thrash — the hypothesis that it was is refuted. At a
fixed arena the order genuinely costs ~10× the WORK at whatever the real diagram size is. With the
node axis dead for this block (GC equilibrium, below), the direct support for it being
**operation-bound rather than size-bound** is exactly this: 48 s versus 525 s with the arena held
constant.

**And `deps` does not degenerate on this block** — it is the first and only case that
distinguishes the two rules:

```
[order] mode="deps" cells=17 groups=3 singletons=2 largest=[15, 1, 1]
```

Three groups, not one. So the attribute idea is genuinely testable here rather than vacuous — and
it **still regresses, 9.6×**. That is worse than "deps degenerates to interleaving": on the single
block where it differs, it does not help. `deps` has now had its chance and should be deleted
unless a new argument appears for it.

(The cell count is **17**, not the 26 that a `state` count in the BTOR2 suggests — the order
operates on kept cone cells. The `MUNUNU_BDD_ORDER_DEBUG` line is authoritative.)

**What the evidence supports, stated precisely.** *"Interleave unless a block says otherwise, and
here is the one that does"* — yes. *"Interleaving is better"* — no. *"It regresses on
operation-bound cones"* — also no: that is one counter-example with no mechanism, and
generalising from it would repeat the error this whole investigation kept making.

**We have no mechanism for the regression.** That is the live cost of flipping: without one we
cannot predict which *other* cones regress, and only 4 of a consumer's 26 blocks have been
measured under interleaving — the 4 largest, which is the right bias for the question but means
1-in-4 of the expensive ones regressed. The opt-out is therefore reactive: a block is discovered
to need it by becoming 10× slower.

## ⚠️ A limit on the peak instrument that these measurements exposed

`approx_num_inner_nodes` counts allocated-**including-dead** nodes. For a cone at GC equilibrium
the reading tracks the **arena**, not the diagram:

| arena | `sdram_burst` peak | occupancy |
|---|---|---|
| 33,554,432 | 28,468,129 | 84.8% |
| 67,108,864 | 60,801,327 | 90.6% |
| 134,217,728 | 118,417,963 | 88.2% |

Occupancy pinned at 85–90% **regardless of size**. For such a block the node budget has no
meaning, and the abstention it would emit — *"the property's cone does not compress to a tractable
BDD"* — would report a garbage-collection artefact as a fact about the property. OxiDD exposes
`gc()`, which removes everything not referenced by a `Function` or another node; calling it before
abstaining and re-reading would make the claim true. Tracked as follow-up.

It also reframes the block: if the live set is small yet a 256 M-arena run took 2h07m, it is
**operation-bound, not size-bound**, and no node budget should be firing on it.
