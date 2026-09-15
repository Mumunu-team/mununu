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

## DECISION (2026-09-14): hold at cell-major

The default **stays `cell-major`**. Both alternative orders remain opt-in. Recorded as a decision,
not an omission, so nobody re-opens it from the win column alone.

**The wins are real and large** — 2.6× across 2,619 tests, 135× on one real test, three consumer
blocks at 115× / 2,458× / 20.2 M×. They were not judged insufficient.

**What blocked the flip is the absence of a mechanism.** One measured block regresses 10.9×, and
nobody can explain why. Without a mechanism there is no way to predict which *other* cones
regress, so an opt-out is REACTIVE — a block is discovered to need it by becoming 10× slower in
someone's lane. And the sample is 4 of a consumer's 26 blocks: the 4 largest, which is the right
bias for the question, and **1 of those 4 regressed.** The remaining 22 are small, not proven safe.

**What would reopen this:** a mechanism for the `sdram_burst` regression — i.e. a property of a
cone that predicts, before running it, that interleaving will cost more work. With that, the
default flips and the predictor picks the order per cone. Without it, the honest claim is only
*"interleave unless a block says otherwise, and here is the one that does"* — which is a reason to
ship the flag, which we have, not to change what everyone gets by default.

`MUNUNU_BDD_VAR_ORDER=interleaved` is the recommended first thing to try on a slow cone.

## The counter-example SURVIVED its test

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

## What would reopen the decision — the literature has a metric for exactly our failure

The missing piece is a **mechanism**: something that predicts, before running a cone, that
interleaving will cost more work on it. Static ordering for symbolic model checking has one.

**Weighted Event Span (WES)** over a *dependency matrix* — rows = transitions/events, columns =
variables, nonzero = that transition reads or writes that variable. Meijer & van de Pol show
**bandwidth and wavefront reduction** minimise WES and thereby reduce **computational effort**;
Cuthill–McKee (1969) and Sloan (1989) compute such orders in milliseconds with standard sparse-matrix
routines, and the DCSH heuristic combines them for this purpose. FORCE and MINCE
(Aloul/Markov/Sakallah) are the min-cut linear-placement relatives.

**Why this metric fits.** WES predicts **effort**, not diagram size — and effort is precisely the axis
`sdram_burst` regressed on: ~10× the work at essentially constant node count. Node counts are
structurally blind to that, which is why the peak instrument explained nothing about that block.

**Why `deps` failing does not refute the approach.** `deps` was *clustering* (union-find over
co-occurrence); the literature does *linear placement*. Different algorithm, and ours was the crude
approximation. The honest statement is "our clustering heuristic failed", not "attribute-derived
ordering failed".

**❌ MEASURED, AND THE FALSIFIER FIRED (2026-09-15).** WES **never picks interleaved** — it ties or
prefers cell-major on every design tested, including the three where interleaving wins by up to
65 537×. The reason is a granularity mismatch, not calibration: `span` is minimised by grouping
correlated variables contiguously, which is what cell-major does by construction, so a
span-minimising metric prefers it almost by definition. And in the literature a *variable* is a whole
state component, whereas ours is a **bit** — any event touching a cell touches all of its bits, so at
bit granularity span is degenerate (1.0000 on every 2-cell case) and cannot see what interleaving
exploits: **correlated bits being adjacent**. What would be needed is a bit-level correlation metric,
which is a different object and not addressed by the surveyed work. Probe:
`probe_w1_weighted_event_span_of_both_orders`.

**The cheap validation, and its falsifier.** Compute WES for the two orders we already have on the two
blocks we already measured. It must rank **cell-major better for `sdram_burst`** and **interleaved
better for `tlm_tx`**. If it does not reproduce measurements we already have, it is not our mechanism.
No reordering algorithm is needed to run this check.

Tracked as the **W-track** in [`.claude/plans/roadmap.md`](../../.claude/plans/roadmap.md).

Sources: [Bandwidth and Wavefront Reduction for Static Variable Ordering in Symbolic Model
Checking](https://arxiv.org/abs/1511.08678) · [MINCE](http://www.aloul.net/Papers/faloul_iwls01_mince.pdf) ·
[FORCE](https://www.researchgate.net/publication/2565569_FORCE_A_Fast_and_Easy-to-Implement_Variable-Ordering_Heuristic) ·
[DCSH / symbolic supervisor synthesis](https://link.springer.com/article/10.1007/s10626-024-00403-4) ·
[Read, Write and Copy Dependencies for Symbolic Model Checking](https://link.springer.com/chapter/10.1007/978-3-319-13338-6_16)

## ✅ DIAGNOSED (2026-09-15): the deciding quantity is APPLY-CALL COUNT, and it is not static

Six falsifiable predictors died on one consumer block (`sdram_burst`): the 10 s wall clock, an
iteration cap, a node cap, the 2-D rule, Weighted Event Span, and selector fraction. **Every one was
computable from the BTOR2 without running anything.** Profiling the block — its lifted BTOR2 supplied
by the consumer — says why.

Enabling OxiDD's per-operation counters (`oxidd-statistics` feature, opt-in) under both orders:

| | cell-major | interleaved | |
|---|---|---|---|
| `sdram_burst` `And` **calls** | 3,815,843 | **36,862,616** | **9.7× MORE** |
| `sdram_burst` `And` hit rate | 35.3% | **45.8%** | *better* |
| `sdram_burst` wall | 7.57 s | 45.79 s | 6.0× |
| barrel shifter `And` **calls** | 201,538 | **2,630** | **77× FEWER** |
| barrel shifter `And` hit rate | 49.6% | **32.1%** | *worse* |
| barrel shifter outcome | — | — | interleaving wins 65 537× |

**The apply-cache hypothesis is REFUTED, and by two directions rather than one.** The hit rate moves
*opposite* to performance in both cases — worse-but-faster on the barrel shifter, better-but-slower
on `sdram_burst`. One direction would have been unsupportive; two make it a refutation.

**What tracks cost is the number of APPLY CALLS** — how many distinct sub-problems the recursion
visits. 77× fewer where interleaving wins, 9.7× more where it loses. The cache is doing its job
either way; the order changes how much work there is to cache.

### Why this explains the six failures instead of joining them

Apply-call count is a property of **the recursion on an order**, not of the model. It does not exist
until you run. So the six deaths were not six bad heuristics — they were **a category error about
where the answer lives**, and a seventh static predictor would die the same way.

### Consequence: measure, do not predict

Run a bounded prefix of the fixpoint under each order, count apply calls, keep the cheaper. Bounded
cost, per-cone answer, no static prediction. **It also implies there may be no right DEFAULT** — only
a cheap measurement made once per cone, which would make holding at cell-major correct permanently
with the order chosen per run.

**Validation available, and it is the check all six skipped:** a consumer holds four wide blocks with
known answers (`tlm_tx` 20.2 M×, `sprite_render` 2 458×, `affine_sampler` 115×, `sdram_burst` 0.09×)
plus 22 small ones with no answer. A probe that picks correctly on the four and does not regress the
22 would be validated on something other than its own motivating case.

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

## SHIPPED (2026-09-15): `MUNUNU_BDD_VAR_ORDER=auto` — opt-in, and not yet shown to pay

> Source of truth: [`ExactModel::build_auto`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs#L319) — surface: CLI-only — an engine-internal ordering knob read from the environment; the verdict it produces is identical under every order (ROBDD canonicity), so there is no API or UI behaviour to expose.

Builds the incumbent order, then races a challenger under a deadline and keeps the cheaper build.
The default is unchanged; nothing picks `auto` unless asked.

### ⚠️ The load-bearing assumption, and it is NOT established: build time proxies solve time

`build_auto` races **build** time and keeps the cheaper **build** — but what the user pays is the
**solve**. On the two cones measured here, bit-blasting alone ranks the orders the same way the full
run does, in *both* directions:

| cone | cell-major build | interleaved build | full-run winner |
|---|---|---|---|
| barrel shifter | 86 ms | 48 ms | interleaved ✓ |
| `sdram_burst` | 1,400 ms | 15,884 ms | cell-major ✓ |

**That is n = 2, and both are the cones that motivated the mechanism.** An earlier draft of this
section, and the commit message at `feat(exact): MUNUNU_BDD_VAR_ORDER=auto`, headlined it as *"the
build phase is enough"* — a universal claim from two self-selected points. That is the **same error
shape as the six predictors it replaced**: validated only on its own motivating case. Corrected here
(monono-45 raised it, 2026-09-15, while the validation was in flight).

**Why correlation is nevertheless expected — as a mechanism, not a result.** Build cost and fixpoint
cost share a common cause: the size of the transition-relation diagram under that order. A larger
relation makes *every* pre-image more expensive, so the two usually move together. But a shared cause
permits divergence in magnitude, and magnitude is all a close call needs to flip.

**The failure it admits is asymmetric, which is the part that matters.** The probe's cost is bounded
at `factor × incumbent_build`. A wrong *pick* is not bounded — it buys the losing order's entire
**solve**. So a cone that is cheap to bit-blast under interleaving but whose fixpoint then visits an
order of magnitude more sub-problems would be chosen wrong, the debug line would still read
`interleaved 300ms < cell-major 900ms — using interleaved`, and the run would get slower with no
signal that anything went wrong. **That is the seventh-failure shape**, and until a measurement rules
it out it is open.

**The wall clock is sound here, and that is not a general licence.** ROBDDs are canonical: the order
changes size and time, never the answer. The worst a mistimed probe can do is choose the slower
order. *A clock may decide COST; it may never decide a VERDICT* — the distinction mununu#553 turned
on, and the one the N-track audits for.

**The gate is a bit count, and the first version was a stopwatch.** Gating on build time **flapped**:
on a noisy host the build straddled the 500 ms threshold, so the same design probed on some runs and
not others, and `auto` came out *worse than both fixed orders* on a small cone. It is now gated on
cone **bit count** (`MUNUNU_BDD_AUTO_MIN_BITS`, default 64) — a property of the design, identical on
every host. A clock is fine for the probe and wrong for the gate, because **the gate is a decision
that should be repeatable.**

### ❌ FALSIFIED (2026-09-15): the consumer sweep — `auto` is 2 of 4, and the failure is architectural

Run by monono-45 on binary `4b6193f`, arena pinned 67,108,864, **wall/CPU ratio 0.97–1.12 on every row**
(so no starved runs), **verdicts identical across all twelve arms** — 24 properties, zero movement, so
canonicity held and no soundness question arises anywhere in this.

| block | cone | gate | `auto` picked | correct? | `auto` | cell-major | interleaved |
|---|---|---|---|---|---|---|---|
| `tlm_tx` | 50 b | **closed** | cell-major | **NO** | 10.3 s | 10.7 s | **0.33 s** |
| `affine_sampler` | ≥64 b | open | interleaved | yes | 10.3 s | 70.7 s | **0.63 s** |
| `sprite_render` | 45 b | **closed** | cell-major | **NO** | 195.7 s | 195.4 s | **0.56 s** |
| `sdram_burst` | 83 b | open | cell-major | yes | **41.0 s** | 46.2 s | 710.2 s |

**Work — apply calls, load- and host-independent, the best evidence in this document:**
interleaved cheaper by **63.6× / 94.9× / 3,653×** on the first three; cell-major cheaper by **13.4×**
on `sdram_burst`.

**1. The gate was the whole failure.** Both wrong picks are cones it refused to look at; both correct
picks are cones it looked at. `sprite_render` is a **45-bit** cone where interleaving is 3,653× cheaper
in work and 348× in wall time — and the gate declined to spend ~1 ms to find that. The `twocount32`
precedent already in this file said a bit count cannot predict cost; `sprite_render` is that sentence
with a price tag.

**2. `auto` is expensive even when RIGHT, and that is architectural.** On `affine_sampler` it picks
correctly and still takes 10.3 s against 0.63 s — **16× the answer it chose** — because
[`build_auto`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs#L319) builds the
incumbent **in full** before racing. The comment claims the probe costs `factor × incumbent_build`;
the true cost of being wrong is **1.0 × incumbent_build plus the probe, paid before any measurement
exists.** You cannot probe your way out of a cost already paid.

**3. The abandoned probe costs multiples of its budget.** `gate=32` on `tlm_tx`: a 4.8 s budget
overran by **+39 s**, a 6.7 s budget by +9 s, wall/CPU 0.99 and 1.00 (so not contention). Run 3 is
explained exactly by its parts (12.4 s build + 0.24 s probe + solve = 13.1 s); run 1 is not. **This is
the arena-state-after-abandonment effect, and it outlives `auto` — any race must abandon a build.**
n=2. OPEN.

**4. And the probe FLAPS — the gate was the wrong half to make deterministic.** Same block, same
settings, three runs: challenger abandoned, abandoned, then succeeded in 240 ms. The gate is
repeatable and *the thing behind it is not*: a deadline of `factor × incumbent_build` is a wall clock,
and **it decides the pick.** Making the gate a bit count while leaving the race a stopwatch does not
deliver "a decision that is a function of the cone."

**5. The ~45% `sdram_burst` tax does NOT reproduce.** `auto` 41.0/41.5/67.5 s against cell-major
46.2/54.0 s — a wash, because the probe dies inside ~300 ms against a ~500 ms incumbent build. **The
earlier 45% figure was measured on a loaded interactive desktop and is struck.**

### DECISION FOLLOWS FROM THE ARITHMETIC, NOT FROM THE COUNT

### ✅ MEASURED (2026-09-15): dropping the gate fixes the PICK perfectly and buys 11%

`sprite_render`, `MUNUNU_BDD_AUTO_MIN_BITS=32`, three runs, **wall/CPU 1.00 throughout**, 7 HOLDS
every run:

| arm | wall |
|---|---|
| `auto` @ gate=64 (never probes) | 195.7 s |
| `auto` @ gate=32 (probes) | 173.8 / 164.9 / 212.5 s |
| plain cell-major | 195.4 s *(and one censored >1583 s — bimodal)* |
| **plain interleaved** | **0.56 s** |

**All 10 cones probed, all 10 picked interleaved, in all 3 runs — thirty decisions, thirty correct,
zero flapping.** So the gate was the *entire* cause of the wrong pick, and lowering it is a complete
fix for the pick.

**It buys 11%, against an available 310×.** One line from the log says why:

```
interleaved 43ms < cell-major 11295ms
```

The probe costs **43 ms** and the incumbent build costs **11.3 s** — and `auto` pays that incumbent
build **once per cone, ten times**, to discover the same answer ten times. Summing run 2's incumbent
builds: **114.5 s of a 164.9 s total, i.e. 69% of the run is building an order it then discards.**

**No configuration of gate or deadline recovers this**, because the cost is in neither. It is in
building the incumbent to completion before measuring anything.

#### ⚠️ Three estimates of this number, and the two corrections both moved it the wrong way

| | value | error |
|---|---|---|
| first estimate (build ≈ 50% of total) | ~100 s | 1.7× optimistic |
| "corrected" (affine_sampler's 14.3% build fraction, applied to this block) | ~28 s | **6× optimistic** |
| **MEASURED** | **174 s** | — |

`sprite_render`'s build fraction is **69%**, not 14.3% — five times the figure used to correct the
first estimate, and *higher* than the 50% the first estimate assumed. So **the correction moved the
number away from the truth, in the direction that made the corrector's own recommendation look
better.** The weakness was named when the correction was sent — one block's ratio applied to another,
the generalisation that has killed seven predictors here — and it stood as a correction anyway.

> **Naming a caveat is not the same as heeding it.** (monono-45's retraction, 2026-09-15.)

Use 174 s. Both estimates are struck.

`auto` is **retired** — now on a measurement rather than on the projection, and the earlier
"flipping the default makes `auto` worse" argument remains **unmeasured and uncitable** (the
incumbent is hardcoded, so no configuration puts interleaved in that role).

**If the race is ever wanted back, exactly one design delivers what was claimed: race on APPLY CALLS,
not wall time.** And this is DEMONSTRATED rather than argued: the counters reproduced
**byte-identically at load 1.7 and at load 4–6** — 573,268 both times on `tlm_tx` interleaved. That is
precisely the property the deadline never had, shown rather than assumed. They are atomics on the hot
path behind an opt-in feature, so the design trades a flapping decision for a permanent runtime tax —
recorded here so nobody reinvents the deadline.

**A related distinction worth keeping.** `tlm_tx`'s peak reproduced to the digit — 20,185,089 against
a ledger entry recorded months earlier on a different engine. So the node axis is **stable** even
though mununu#553 showed it is not **predictive**. Those are different failures and only the second is
fatal; a stable-but-non-predictive quantity is still usable as a regression check.

### ⚠️ THREE OF FOUR CONSUMER BLOCKS ARE BIMODAL IN SOMETHING — the most portable finding of the day

Nobody set out to measure this, and it is more reusable than the ordering result that occasioned it.

| block | bimodal in | measured |
|---|---|---|
| `sdram_burst` | total run time | 450 s / 6,180 s, identical settings |
| `sprite_render` (cell-major) | total run time | 195.4 s / censored >1583 s, wall/CPU 1.00 both |
| `tlm_tx` | **the interleaved BUILD itself** | 240 ms / >4,791 ms, identical settings |

**`tlm_tx`'s is the strange one and it outlives `auto`.** At `gate=32` its challenger was abandoned
twice and succeeded once — and the success took **240 ms against a 4,791 ms budget**, i.e. **20×
inside the deadline**. So the two abandonments cannot be the deadline logic: the interleaved build's
own cost is bimodal on that block. `sprite_render` shows nothing of the kind (33–247 ms across 30
probes, tight), so it is `tlm_tx`-specific.

**A single timing from any of these three is worthless.** Two runs per arm minimum, wall/CPU recorded
beside each.

#### ❓ An open question this raises about the apply-call race

monono-45's reading is that an apply-call race would hit `tlm_tx`'s bimodality too, and would hide it
better, "because the counter would simply come back large." **Their own earlier measurement appears to
refute that:** apply calls on `tlm_tx` interleaved came back **byte-identical (573,268) at load 1.7
and at load 4–6**. If the work is invariant while the build *time* varies 20×, then the bimodality is
**not in the work** — it is in allocation, GC, or arena/page state — and a work-based race would see
the same count both times and choose correctly.

Unresolved, and it matters, because the apply-call race is the one design recorded above as both
measuring and repeatable. The discriminating measurement is apply calls for the *build phase alone*
across a bimodal pair on `tlm_tx`; the byte-identical figure was taken over a full run. Nobody has
run it.

### ⚠️ A SECOND BIMODAL BLOCK, and the most reusable finding of the day

`sprite_render` under cell-major: **195.4 s on one run, censored >1583 s on the next**, wall/CPU 1.00
on both, identical command. `sdram_burst` was already known bimodal (450 s then 6,180 s at identical
settings). **Two blocks in this corpus where a single timing is worthless** — which is why the sweep
used two runs per arm, and why any future measurement here does the same and records wall/CPU beside
every number.

### Honest state: validated for correctness, not for benefit

Three runs each, idle host, after the gate fix:

| cone | cell-major | interleaved | `auto` |
|---|---|---|---|
| barrel shifter, 24 bits *(under the gate)* | 0.53–0.59 s | **0.23 s** | 0.55–0.80 s |
| `sdram_burst`, 83 bits | **7.60–7.75 s** | 45.6–55.3 s | 10.8–11.4 s |

**On this corpus `auto` is pure cost.** It taxes `sdram_burst` ~45% for a choice that was already
right, and the one design where interleaving wins sits below the gate. Its value can only appear on a
cone that is **both** above the gate **and** helped by interleaving — and this repository has no such
design. Nothing here demonstrates a benefit, and the shipped code claims none.

The consumer does have them: `tlm_tx`, `sprite_render` and `affine_sampler` are all wide *and*
interleaving-favouring, plus `sdram_burst` as the negative control and 22 small blocks with no known
answer. **Those four figures are not the same kind of measurement, and an earlier draft presented
them as though they were:** 20.2 M× / 2 458× / 115× are peak **node counts** of the built diagram (a
build-phase property), while `sdram_burst`'s 0.09× is **total run time** (48 s vs 525 s at a fixed
67 M arena). Mixing the axes is exactly how the build-proxies-solve assumption above stayed invisible.
The validation therefore measures **total wall time under all three orders on all four**, which is the
axis a user actually pays, and compares auto's choice against which fixed order genuinely finished
first. **A probe that picks correctly on the four and does not regress
the 22 would be validated on something other than its own motivating case** — the check all six
failed predictors skipped. Until that runs, this is opt-in machinery with a mechanism behind it and
no demonstrated win.

```
MUNUNU_BDD_VAR_ORDER=auto MUNUNU_BDD_ORDER_DEBUG=1 mununu btor2 verify-recoverability ...
  # 24-bit cone -> "not probing: under the 64-bit gate"
  # 83-bit cone -> "cell-major kept (challenger abandoned)"
```
