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
| fixpoint node guard | **none** | `MUNUNU_BDD_FIXPOINT_NODES`, default 2 M, `min(cap, node_budget)` |
| abstention determinism | host-dependent | **deterministic** — measured: bail at fixpoint step 1720 on all 3 runs while durations moved 8% |

Setting `MUNUNU_BDD_TIME_BUDGET_MS=0` keeps working and now simply names the default. A positive value still gives you a hard time ceiling, and its abstention message now **says in words** that the result is non-deterministic and that a faster machine may decide the same property.

## Direction of change — read this bit

- `unknown` → `holds`, or `unknown` → `violated`.
- **No `holds` becomes `violated`, and no `violated` becomes `holds`.**

An abstention is the engine declining to answer; removing a reason to decline can only turn ⊥ into a verdict. Measured on monono's 71-check lane, three properties moved and all three moved toward decided — including an `AG EF (st_q == S_IDLE)` recoverability guarantee on a block whose contract had read *"TIER 3 IS THEREFORE NOT ESTABLISHED FOR THIS BLOCK"* since the card shipped. It was established all along; a stopwatch was withholding it.

## Why node count and not a diameter/iteration bound

Both we and monono first proposed a cone-aware **iteration** bound. Measuring it refuted us, and the refutation is worth carrying because it is counter-intuitive:

| cone | iters | µs/iter | peak nodes | nodes/iter | |
|---|---|---|---|---|---|
| wrapping counter, depth 8064 | 8064 | 5.5 | 25 959 | 3.2 | **decides** |
| `uart_msg_handler` `AG EF` (real RTL) | 60 | — | 524 289 | 8 738 | **decides** |
| `twocount32`, 4096 steps in | 4096 | 26 059 | 4 999 635 | 1 221 | never decides |

A deep cone is **cheap** and flat in depth; the pathological one gets 12.7× more expensive as its reach-set grows without bound. So an iteration cap low enough to bail `twocount32` in seconds sits near ~1000 — on top of the ~800-step diameters that legitimately decide. Growth *rate* fails too (`uart` decides at 8738 nodes/iter; `twocount32` never does at 1221). Absolute node count separates them, and it is **deterministic**: across two runs the counts were byte-identical while wall times moved 12%.

## What to expect, and what to do

**monono.** Your lane already runs `MUNUNU_BDD_TIME_BUDGET_MS=0`, so the default change is a no-op for you — you have already measured its effect (71/71 green, three recovered verdicts, one block at 918 s whose per-block budget you doubled to 1800 s). What is **new** for you is the node budget: a cone peaking above 2 M will now abstain where previously it ground on. **Run the lane with `MUNUNU_BDD_REPORT_PEAK=1` before adopting** and send the per-block peaks — the 2 M default is calibrated on a corpus whose largest decider is 590 k, and your SDRAM-schedule cones may exceed it. If they do, the default moves before this ships to you.

**ROSF.** No action unless you set `MUNUNU_BDD_TIME_BUDGET_MS` explicitly. Expect some previously-`unknown` properties to decide, and expect the exact engine to spend longer before abstaining on a hopeless cone (~14 s vs ~10 s). If you pin verdicts, re-baseline.

**Anyone pinning `unknown`.** A pin that asserts `unknown` for a property whose cone fits may now fail. That is the fix working; re-pin to the definite verdict.

**Sizing the budget.** `MUNUNU_BDD_REPORT_PEAK=1` prints one line per evaluation — take the **max per block**, not the sum, since a re-planned property evaluates more than once. Peaks are accounted in ~2^16 chunks, so read them to the nearest 65536 (`peak 1` means "under one chunk"). To raise the real ceiling you must raise `MUNUNU_BDD_ARENA_NODES` alongside `MUNUNU_BDD_FIXPOINT_NODES`: the budget is applied as `min(cap, node_budget)` precisely so a fixpoint cannot exhaust the OxiDD arena, which would trade a clean abstain for an uncatchable `SIGABRT`.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust-only change in `mununu-core`; no toolchain or subprocess-tool change | **No** |
| `mununu-sva` | Inherits `mununu-dev`; slang/sv2v/yosys pins unchanged | **No** |
| `mununu-sva-pono` | Inherits; Pono/MathSAT pins unchanged | **No** |
| `hw-verif` | Verilator-side only; untouched | **No** |

Rebuild only to pick up the new binary in the usual way — no image-definition change is involved.

## Honest limits

- **The 2 M default is provisional.** It is ~3.4× over the largest decider we could measure (590 k, `uart_msg_handler`). That is thin, and the cones most likely to exceed it are consumers', not ours. It is env-overridable and will be revised on monono's 26-block sweep.
- **A hopeless cone now grinds ~40% longer** before abstaining (~14 s vs ~10 s at the default). The wall-clock backstop belongs in the harness, where a timeout is reported *as* a timeout and names the block.
- **This does not fix [mununu#543](https://github.com/Mumunu-team/mununu/issues/543)** — the OxiDD `apply_bin` stack overflow is a separate, still-open defect in the same block report.

## Not covered here

- The per-property budget monono floated (a budget proportional to cone size rather than a global constant). Deferred pending the sweep, which is the measurement that decides whether a constant is honest.
- Naming which engine and which budget produced a ⊥ in the *report* (as opposed to the engine's message) — that is [mununu#548](https://github.com/Mumunu-team/mununu/issues/548).

## Provenance

- Fix: `acc48e5` (`fix(exact): a deterministic node budget replaces the wall-clock default`), on `fix/553-deterministic-fixpoint-budget`.
- Instrument, separately cherry-pickable onto `main`: `5ae558b`.
- Issue: [mununu#553](https://github.com/Mumunu-team/mununu/issues/553).
- Measurement: `probe_553_iteration_cost_by_cone_shape` (`#[ignore]`d) in `crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
