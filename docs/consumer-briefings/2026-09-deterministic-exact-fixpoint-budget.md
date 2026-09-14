# Consumer briefing — 2026-09 the exact engine's wall-clock default is OFF; verdicts no longer depend on the host

> **Audience:** monono, ROSF, anyone running the `exact-symbolic` engine — `sv verify-auto`, `btor2 verify*`, or the API peers.
>
> **Related:** [mununu#553](https://github.com/Mumunu-team/mununu/issues/553). Sibling of, but independent from, [mununu#543](https://github.com/Mumunu-team/mununu/issues/543) — the two failures were reported in one block and are separate defects.
>
> **TL;DR:** the exact engine used to abstain on a **10 s wall clock by default**, so the same command could DECIDE on a fast machine and report `unknown` on a slow or busy one — and nothing in the report told those two ⊥ apart. The clock is now **off by default**, replaced by a deterministic live-node budget. **Properties that used to return `unknown` will now decide.** No definite verdict flips.

## What was wrong

`ExactModel`'s fixpoint carried a `deadline` defaulting to `MUNUNU_BDD_TIME_BUDGET_MS=10000`. A property whose cone needed more than 10 s abstained — and a wall clock is not a property of the design, it is a property of the machine. Same command, same engine, same design, different answer on a busier host.

The root cause makes the clock's existence legible. `check_node_budget` is a `BddBitBlaster` method guarding the **bit-blasting ops**; `ExactModel` held **no manager reference at all**. So the μ-fixpoint — the phase that actually blows up on a cone that does not compress — ran with **no node guard whatsoever**. The clock was standing in for a budget that was never wired. This wires it.

## What changed

| | before | after |
|---|---|---|
| `MUNUNU_BDD_TIME_BUDGET_MS` | default `10000` | default **`0` (off)**; opt-in only |
| `MUNUNU_BDD_ARENA_NODES` | wide tier only, `1<<25` | **both tiers**, `1<<24` / `1<<26` |
| fixpoint node guard | **none** | `MUNUNU_BDD_FIXPOINT_NODES`, default **80% of the arena** |
| abstention determinism | host-dependent | **deterministic** — measured: bail at fixpoint step 1720 on all 3 runs while durations moved 8% |

Setting `MUNUNU_BDD_TIME_BUDGET_MS=0` keeps working and now simply names the default. A positive value still gives you a hard time ceiling, and its abstention message now **says in words** that the result is non-deterministic and that a faster machine may decide the same property.

## Direction of change — read this bit

- `unknown` → `holds`, or `unknown` → `violated`.
- **No `holds` becomes `violated`, and no `violated` becomes `holds`.**

An abstention is the engine declining to answer; removing a reason to decline can only turn ⊥ into a verdict. Measured on monono's 71-check lane, three properties moved and all three moved toward decided — including an `AG EF (st_q == S_IDLE)` recoverability guarantee on a block whose contract had read *"TIER 3 IS THEREFORE NOT ESTABLISHED FOR THIS BLOCK"* since the card shipped. It was established all along; a stopwatch was withholding it.

## No hand-picked budget survived — what the guards actually do now

Four candidate budgets were proposed and each was refuted by the SAME consumer property —
`a_frame_wraps_at_total`, nine lines of SVA about a 640×480 raster, now reproduced in-repo
(`probe_553_raster_wrap_is_deep_and_wide`) to within 0.1% at **7.98 M nodes in 818 626 iterations,
and it DECIDES**:

| candidate | what it does to that property |
|---|---|
| wall clock, 10 s default | abstained — the original defect |
| iteration/diameter cap ~1000 (low enough to bail a free counter fast) | cuts it at 818 626 |
| live-node cap 2 M | 3.8× over |
| 2-D `nodes AND iters` | exceeds BOTH thresholds whenever the free counter does |

The reason is not calibration. In a 26-block / 536-reading sweep, **deciders and non-deciders overlap
by 6.2×** in node count: a 31.0 M-node cone decides, a 5.0 M-node one never does. No scalar orders
them, because separating them from a prefix of the fixpoint means predicting convergence — and the
two are indistinguishable at iteration 1000, the difference being convergence at 818 626 versus ~2³².

**So the budgets stopped trying to predict decidability.** What each one does now:

- **Fixpoint node budget** — a RESOURCE guard only: stop the fixpoint exhausting the OxiDD arena.
  80% of the arena, no hand-picked constant.
- **Iteration budget** (`1<<20`) — the deterministic total-work bound. It is the only one of the four
  that gets both measured cases right (raster at 78%, free counter 4096× over).
- **Fast answers on a hopeless cone** — the PORTFOLIO's job, and it already does it:
  `decide_reach_owned_only` returns on the first definite verdict without joining slow members.
- **Wall-clock ceilings** — the HARNESS's job, where a timeout is reported *as* a timeout and names
  the block.

## ⚠️ Arena defaults raised — read this even if nothing else

`MUNUNU_BDD_ARENA_NODES` is now honoured in **both** tiers (it was read only for cones >40 bits, so a
≤40-bit cone that outgrew the fixed arena had no escape hatch at all), and the defaults rose:

| tier | before | after | baseline RSS |
|---|---|---|---|
| ≤40 cone bits | `1<<23` (8 M) | `1<<24` (16 M) | 70 → 110 MB |
| >40 cone bits | `1<<25` (33.5 M) | `1<<26` (67 M) | 190 → 350 MB |

**Why, and it is a latent-abort fix, not tuning.** Real DECIDING consumer cones were measured at
**31.0 M nodes against a 33.5 M arena — 92.5% occupancy**. The headroom below the arena is what
absorbs a single wide op's allocation after the last between-op check; at 92.5% the next
slightly-wider cone does not abstain, it exhausts the arena inside one apply, and the allocation
`.unwrap()` panics while the exhausted manager is dropped — `catch_unwind` cannot save it and the
process aborts. The raise drops those cones to 58%.

## What to expect, and what to do

**monono.** Your lane already runs `MUNUNU_BDD_TIME_BUDGET_MS=0`, so the clock default is a no-op
for you. Three things that are not:

1. **The arena raise is the one to adopt first** — it is a latent-abort mitigation for `sprite_render`
   and `sdram_burst` at 92.5% occupancy, and it applies on a released engine independently of
   anything else here.
2. **Check the tier each block lands in.** `MUNUNU_BDD_REPORT_PEAK=1` now prints the budget, so a
   block showing `budget 53687091` is on the wide tier and one showing `budget 13421772` is on the
   small. Any block above 13.4 M that reports the *small* budget would abstain — none is expected,
   but it is a one-line check rather than an assumption, and it is the acceptance test for this
   change on your corpus.
3. **The iteration budget is your next wall and you are at 78% of it.** Measured law:
   `iters ≈ 1.95 × (h·v)` — the fixpoint traverses the FRAME, not the line, which is why the
   "~800-step diameter" estimate was off by three orders of magnitude. 720p ≈ 230% and 1080p ≈ 460%
   of the `1<<20` default. Raise `MUNUNU_BDD_ITER_BUDGET` pre-emptively on raster designs.

**ROSF.** No action unless you set `MUNUNU_BDD_TIME_BUDGET_MS` explicitly. Expect some previously-`unknown` properties to decide, and expect the exact engine to spend longer before abstaining on a hopeless cone (~14 s vs ~10 s). If you pin verdicts, re-baseline.

**Anyone pinning `unknown`.** A pin that asserts `unknown` for a property whose cone fits may now fail. That is the fix working; re-pin to the definite verdict.

**⚠️ A peak at or near the arena is a LOWER BOUND, not a measurement.** `approx_num_inner_nodes`
counts allocated-including-dead nodes, so an arena smaller than the cone needs forces GC, and GC
reclaims dead nodes — the high-water mark comes out LOW, and non-monotonically. Measured on one
design at a fixed property, varying only the arena: 801,404 / 526,547 / 1,245,185 at arenas of
1.05M / 1.31M / 1.64M, then stable at 1,638,401 from 2.10M upward. **Raise
`MUNUNU_BDD_ARENA_NODES` until the reading stops moving; only then read it as the cone's size.**
This is not hypothetical — a consumer's peak table taken at a 33.5M arena understated three of its
four largest blocks, two by more than 2×, and the only block that read correctly was the one whose
true peak fit the arena.

**Sizing the budget.** `MUNUNU_BDD_REPORT_PEAK=1` prints one line per evaluation — take the **max per block**, not the sum, since a re-planned property evaluates more than once. Peaks reproduce across runs for a fixed design+property+config, but to about 0.01% rather than to the byte — one consumer property measured 7,980,029 / 7,979,366 / 7,979,343 across three runs while a second property in the same runs was byte-identical, and the iteration counts were identical every time. Size a budget with headroom, not to the exact number. `peak 1` means the fixpoint saturated to ⊤ (a terminal node). To raise the real ceiling you must raise `MUNUNU_BDD_ARENA_NODES` alongside `MUNUNU_BDD_FIXPOINT_NODES`: the budget is applied as `min(cap, node_budget)` precisely so a fixpoint cannot exhaust the OxiDD arena, which would trade a clean abstain for an uncatchable `SIGABRT`.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust-only change in `mununu-core`; no toolchain or subprocess-tool change | **No** |
| `mununu-sva` | Inherits `mununu-dev`; slang/sv2v/yosys pins unchanged | **No** |
| `mununu-sva-pono` | Inherits; Pono/MathSAT pins unchanged | **No** |
| `hw-verif` | Verilator-side only; untouched | **No** |

Rebuild only to pick up the new binary in the usual way — no image-definition change is involved.

## Honest limits

- **A hopeless cone now grinds longer** before abstaining — the fixpoint runs to 80% of the arena
  rather than to a 10 s clock. This is deliberate: the portfolio returns on the first definite
  verdict regardless, so the grind is not on the critical path, but it does consume CPU and arena in
  the background until it finishes.
- **The arena raise costs memory for everyone** (+40 MB small tier, +160 MB wide), not only for the
  cones that needed it. Set `MUNUNU_BDD_ARENA_NODES` down if a deployment is memory-constrained and
  its cones are known small.
- **⚠️ And it costs TIME on already-heavy cones — ~2.1× per arena doubling.** Measured n=3 per
  setting on a verified-idle host (0 competing processes checked before AND after), with the
  pass count / verdict confirmed on every run:

  | workload | arena 33.5 M | arena 67 M | |
  |---|---|---|---|
  | BDD-heavy recoverability test (decides either way) | 53.6 s mean | 114.8 s mean | **2.14×** |
  | fast real cone (`uart_msg_handler`, 524 k peak, 60 iters) | 3.2–5.3 s | 3.5–3.6 s | **no change** |
  | the whole `mununu-core` lib suite | ~120 s | 130.6 s | **+9 %** |

  So the cost is NOT universal: ordinary properties and the gate as a whole are barely affected,
  and only cones already doing heavy BDD work pay the 2×. But a block already near a per-block
  timeout is exactly that profile, so **time any long-running block with and without the raise
  before adopting it lane-wide** — the arena setting and a per-block timeout are coupled, not
  independently tunable.

- **Calibrated on both designs and contrast twins.** Twin max 31,014,613 vs design max 31,048,833 —
  same ceiling. This mattered because the two populations are uncorrelated: `affine_sampler`'s twin is
  **4,600× smaller** than its design (4,941 vs 22,867,186) while `video_timing`'s is **1,100× larger**
  (7,980,029 vs 7,250). If you calibrate a budget for your own corpus, **measure twins too** — a
  design-only set is not conservative, it is uncorrelated.

- **We cannot predict which cones will abstain.** That is the finding, not an omission: no
  cone-intrinsic scalar separates decidable from undecidable here. `MUNUNU_BDD_REPORT_PEAK=1` is how
  you find out, per block, by measuring.
- **This does not fix [mununu#543](https://github.com/Mumunu-team/mununu/issues/543)** — the OxiDD
  `apply_bin` stack overflow is a separate, still-open defect reported in the same block.

## Not covered here

- The per-property budget monono floated (a budget proportional to cone size rather than a global constant). Deferred pending the sweep, which is the measurement that decides whether a constant is honest.
- Naming which engine and which budget produced a ⊥ in the *report* (as opposed to the engine's message) — that is [mununu#548](https://github.com/Mumunu-team/mununu/issues/548).

## Provenance

- Fix: `acc48e5` (`fix(exact): a deterministic node budget replaces the wall-clock default`), on `fix/553-deterministic-fixpoint-budget`.
- Instrument, separately cherry-pickable onto `main`: `5ae558b`.
- Issue: [mununu#553](https://github.com/Mumunu-team/mununu/issues/553).
- Measurement: `probe_553_iteration_cost_by_cone_shape` (`#[ignore]`d) in `crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
