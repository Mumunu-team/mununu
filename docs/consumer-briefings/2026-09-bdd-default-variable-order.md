# Consumer briefing — 2026-09 the exact engine's default BDD variable order is now `interleaved`, and `auto` is retired

> **Audience:** monono, ROSF, anyone invoking the `exact-symbolic` engine — `mununu btor2 verify` / `verify-liveness` / `verify-recoverability`, or `sv verify-auto` (which calls it directly).
>
> **Related:** [mununu#553](https://github.com/Mumunu-team/mununu/issues/553) and its ordering follow-up. Design record: [`docs/design/bdd-variable-ordering.md`](../design/bdd-variable-ordering.md).
>
> **TL;DR:** **no verdict changes — ROBDDs are canonical, so the variable order changes size and time and never the answer** (24 properties measured across twelve arms, zero movement). What changes is RUNTIME, in both directions: three of four measured consumer blocks get **63.6× / 94.9× / 3,653×** cheaper in apply calls; one — **`sdram_burst`, ~46 s → ~710 s** — gets 15.4× more expensive and **must be pinned to `cell-major`**. `MUNUNU_BDD_VAR_ORDER=auto` is retired and is now a no-op that warns.

## What changed

| | before | after |
|---|---|---|
| default order | `cell-major` | **`interleaved`** (bit-position-major) |
| opt-out | `MUNUNU_BDD_VAR_ORDER=interleaved` | **`MUNUNU_BDD_VAR_ORDER=cell-major`** |
| `…=auto` | raced two builds, kept the cheaper | **retired** — no-op, warns, uses the default |
| `…=deps` | already removed | unchanged (removed) |
| `MUNUNU_BDD_AUTO_MIN_BITS`, `MUNUNU_BDD_AUTO_FACTOR` | gated/tuned the chooser | **deleted** — see the silent-failure warning below |
| an unrecognised value | silently used the default | **warns, and says it is a CONFIG ERROR** |

Canonical spelling is **`cell-major`**; `cell_major` and `cellmajor` are accepted aliases, all case-insensitive. Use the hyphenated form in anything you commit.

## Direction of change — read this bit

**Verdicts do not move.** This is not a bugfix and not a semantics change. A ROBDD is canonical: for a fixed set of variables, the order changes the diagram's size and the time to build and explore it, never the function it represents. Measured across all twelve arms of the validation sweep: 24 properties, **identical verdicts everywhere**.

**Runtime moves a lot, and not all one way.** Measured in **apply calls** — the count of distinct sub-problems the apply recursion visits, which is load- and host-independent (it reproduced *byte-identically* at load 1.7 and at load 4–6):

| block | cone | cheaper order | factor |
|---|---|---|---|
| `sprite_render` | 45 b | interleaved | **3,653×** |
| `affine_sampler` | ≥64 b | interleaved | **94.9×** |
| `tlm_tx` | 50 b | interleaved | **63.6×** |
| **`sdram_burst`** | 83 b | **cell-major** | **13.4×** |

Wall times at a pinned 67,108,864-node arena, wall/CPU ratio 0.97–1.12 on every row (so no starved runs): `sprite_render` 195.4 s → **0.56 s**; `affine_sampler` 70.7 s → **0.63 s**; `tlm_tx` 10.7 s → **0.33 s**; `sdram_burst` 46.2 s → **710.2 s**.

The flip is justified on that distribution, not on a count: the wins are two orders of magnitude larger than the single loss, every synthetic shape agrees, the whole `mununu-core` lib suite goes 123.8 s → 48.2 s, and **cell-major's worst case on a real block is unbounded** — `sprite_render` measured 195.4 s on one run and was censored past **1583 s** on the next at identical settings.

## ⚠️ `sdram_burst` — pin `cell-major`, and the pin is load-bearing

**This is the one action item.** Export it around **that block's own invocations only**:

```sh
# rtl/mem/sdram_burst/verify.sh  — NOT the Makefile, NOT the docker run
export MUNUNU_BDD_VAR_ORDER=cell-major   # removing this converts a bimodal block
                                         # into a false engine defect — see below
```

### ⛔ Do NOT set it at the container or lane level

The natural place to put an engine setting is alongside the others in the `docker run`:

```sh
docker run -e MUNUNU_BDD_TIME_BUDGET_MS=... -e MUNUNU_BDD_ARENA_NODES=...   # <- NOT here
```

That is **lane-global**, and it would pin `cell-major` for all 26 blocks — on a corpus where three
of the four wide ones are **63.6× / 94.9× / 3,653×** cheaper interleaved. You would trade a 3,653×
win on `sprite_render` to protect one block. The variable is read per process and monono runs **one
process per check** (`verify/lib.sh:155`), with `sdram_burst` contributing three of them, so
per-block export is exactly the granularity available. (Placement identified by monono-8d before
it was made.)

**Why it is not a performance tweak.** `sdram_burst` is **bimodal** — 450 s and 6,180 s measured on *identical* settings, a 13.7× excursion. Under the new default its median moves from ~46 s to ~710 s, which against monono's 1800 s per-block budget is ~40% and leaves it **one bimodal excursion from crossing**. A crossing does not present as "slow": it presents as `TIMEOUT — not a verdict`, which fails the lane and invites a bug hunt into the engine.

So the pin is what stands between that block and a **spurious engine finding**. Record that reason next to it, or the next reader will see a performance pin and feel free to delete it.

**We are deliberately not auto-detecting this.** A shift-dense-cone detector would be the eighth static predictor for this quantity; seven have died on this exact block, including the retired chooser's own bit-count gate. The deciding quantity is apply-call count, which does not exist until the recursion runs.

## Why `auto` is retired

It picked correctly on **2 of 4** consumer blocks, and the failure was architectural rather than a tuning fault:

1. **The gate.** It refused to probe cones under 64 bits. Both wrong picks were gated cones — `sprite_render` is a **45-bit** cone where interleaving is 3,653× cheaper and the gate declined to spend ~1 ms to find that.
2. **It builds the incumbent in FULL before racing.** So a wrong pick costs `1.0 ×` the wrong build *before any measurement exists* — and a **correct** pick still does: `affine_sampler` is `auto` choosing right and landing at 10.3 s against 0.63 s, **16× the answer it chose**.
3. **The pick flapped.** Identical settings, three runs: challenger abandoned, abandoned, then succeeded in 240 ms. The gate was deterministic; the deadline behind it was not, and the deadline decided the pick.

If the mechanism is ever revisited, the only design that is both measuring and repeatable is to race on **apply calls**, not wall time. Recorded in the design note.

### ⚠️ Silent-failure warning for anyone scripting the retired flags

`MUNUNU_BDD_AUTO_MIN_BITS` and `MUNUNU_BDD_AUTO_FACTOR` are **deleted, not defaulted**. A script setting them now has no effect and says nothing — the run measures the new default while appearing to exercise the chooser. `MUNUNU_BDD_VAR_ORDER=auto` *does* warn. If you have runs pinned to the old behaviour for comparison, pin the **binary** (`4b6193f`), not the flag.

## Who is affected

- **monono** — the formal lane adopts this on the next engine bump. One config change (`sdram_burst`, **per-block, not lane-global** — see the placement warning), and three blocks get dramatically faster. Nothing else to do.
- **ROSF** — affected only through runtime; no verdict or report-shape change.
- **mununu-ui** — not affected. No wire-format or type change.
- **Anyone pinning expected verdicts** — no row moves. This briefing fires on runtime and defaults, not semantics.

## Report-parsing impact

**None.** No JSON shape change, no new or removed verdict value, no CLI flag added or removed on any surface. The only new output is on **stderr**: a warning for `…=auto` and a warning for an unrecognised value.

## Verification

- 24 properties × twelve arms on four wide consumer blocks, arena pinned, **verdicts identical in every arm**.
- Apply-call counts (`oxidd-statistics`) as the primary metric, reproducing byte-identically across a 3× load range.
- `mununu-core` lib suite, 2,619 tests, 0 failures under both orders.
- The multiplication control (`a * b == 1`) does **not** improve under interleaving — as Bryant 1986 requires — which is what licenses reading the other rows as an ordering effect rather than a harness artefact.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | default engine runtime | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump; e2e runs here | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no exact-engine path | No |
| rosf | runtime only, no verdict change | **No** (rebuild to get the speedup) |
| monono Docker | formal lane runs the exact engine | **Yes**, and pin `cell-major` for `sdram_burst` in the same change |
| mununu-ui | no type or wire change | No |

## For monono

Three of your four wide blocks improve by 63.6× / 94.9× / 3,653× in work. The single action is the `sdram_burst` pin above, and it is **load-bearing** for the reason in that section — not a tuning preference.

Two things from your own instrumentation that this briefing depends on, and which are worth keeping:

- **Two blocks in this corpus are bimodal** — `sdram_burst` (450 s / 6,180 s) and `sprite_render` (195.4 s / censored >1583 s), both at identical settings. **A single timing from either is worthless.** Two runs per arm minimum.
- **Record `wall/cpu` beside every timing, and beside every TIMEOUT.** ~1.0 means the run had the cores it needed; ≫1 means it was starved and the number is worthless. This matters for a `timeout`-gated lane specifically: it is what distinguishes *too hard* from *starved*, which are different bugs with different owners. `/usr/bin/time -l`'s involuntary context switches are better still, since they attribute preemption to the process rather than to the machine.

## Not covered here (follow-ups)

- **An abandoned build leaves the arena in a worse state than a clean run** — measured overruns of +39 s and +9 s against 4.8 s and 6.7 s probe budgets, wall/CPU 0.99 and 1.00 (so not contention). `n=2`, unexplained, and it **outlives the chooser** because any future race must abandon a build. OPEN.
- **`approx_num_inner_nodes` counts allocated-INCLUDING-DEAD nodes**, so at GC equilibrium it reports the *arena* rather than the diagram (measured 84–90% occupancy across 33 M / 67 M / 134 M arenas on the same block). Do not size anything off it. The fix — `gc()` before reading — is tracked.
- **Dynamic reordering (sifting) is not implemented.** `oxidd-reorder` ships the adjacent-swap primitive and `set_var_order`, but no sifting heuristic. Under evaluation; note that sifting minimises *size*, and `sdram_burst` has near-equal diagrams with 13.4× the work, so size and cost provably diverge exactly where a chooser is most needed.
