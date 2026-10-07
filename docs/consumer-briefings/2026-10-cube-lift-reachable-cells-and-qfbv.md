# Consumer briefing — 2026-10 the explicit cube lift is 6× faster; `btor2 cegar` counts only the cells it decided

> **Audience:** monono, ROSF, and any consumer of `btor2 cegar --engine explicit` (CLI JSON or `POST /api/v1/btor2/cegar`), or that runs the explicit predicate-cube lift through the verify orchestrator.
>
> **Provenance:** category 2 (algorithmic) of the engine-performance roadmap, items A3 and A4. Measured on the i2c lift at |P| = 8 (`scripts/profile_cases.py`, case `cube-rtl-i2c`). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

Same verdict at the initial states, 6× faster lift, and the CEGAR verb's whole-space tallies change shape: cells the lift did not decide are now reported in their own count (`unlifted_cells`) instead of being counted as decided (before) or as "needs refinement".

| `btor2 cegar --engine explicit`, i2c lift, 8 control predicates, 256 cells | before | after |
|---|---|---|
| wall, reset pinned (`--config-values` on the predicate registers) | 15.0 s | **2.9 s** |
| `verdict`, reset pinned | `{true 0, false 256, unknown 0}` | `{true 0, false 92, unknown 0, unlifted 164}` |
| `outcome`, reset pinned | "PROPERTY VIOLATED — 256 cell(s) falsify the formula" | "PROPERTY VIOLATED — 92 cell(s) falsify the formula" |
| wall, reset free (the raw yosys lift: no `init` lines, no config values) | 15.0 s | 8.9 s (A3 only — every cell is initial, nothing is unreached) |
| counterexample, `terminated_with`, iteration count | unchanged | unchanged |

**Correction (2026-10-07, after merge).** The first version of this briefing and PR #610 reported "72 of 256 cells reachable, 8.9 → 2.3 s" on the *raw* i2c lift. That was measured on the A4 branch before #603 merged; #603 made a register with no `init` line **free at reset** (the sound reading), and the raw yosys lift has no `init` lines — so under the merged semantics every cell is initial, the roots are the whole cube, and A4 lifts all 256 cells there (correct, and no faster than A3). A4's saving appears when the reset is pinned — `init` lines, or `--config-values` / the sidecar on the predicate registers, which is what `verify-auto`'s reset detection supplies — and is then 92 of 256 cells on this lift. The numbers above are the merged behaviour.

## What changed

### A3 — cheaper z3 queries in the post-image all-SAT loop

- z3's `model.compact` is set to `false` **process-wide**, once. The loop asks z3 for a full model after every `Sat` and reads |P| Booleans from it; compressing that model (an occurrence walk, a dependency top-sort and a clean-up rewrite) was a quarter of `get_model`'s 26 % of the lift. Compression only matters to code that walks a model's structure; every z3 model consumer in mununu evaluates terms against the model instead, so nothing reads differently anywhere. Side effect for every other z3 user in the process (native BMC, SPACER, interpolation, must-edge inference): model extraction is cheaper there too.
- On a design without memories (the BvOnly theory) the loop's solver is `Solver::new_for_logic("QF_BV")` — z3's bit-blast + SAT-core tactic — instead of the generic solver, which routed every query through the `smt` core. With memories (BvUfArray) nothing changes.

Measured: 15.0 → 13.0 s (compact off) → 10.1 s (QF_BV) → 8.9 s (both), the lift byte-identical in every arm.

### A4 — the verdict path lifts only the cells the initial cubes reach

The post-image loop used to compute post-images for all 2^|P| source cubes. The verdict at the initial states depends only on the cells forward-reachable over may-edges (every modality reads successors; must ⊆ may), so the loop is now a worklist BFS when the caller sets `PredicateCubeLiftOptions::reachable_only`. Its roots are the KMTS's declared initial cubes **plus every cube consistent with the design's `init` lines** (`reachable_lift_roots`): the declared initial state is the cube_0 placeholder without config values (mununu#609), and the recoverability ladder reads the trace at the design's real reset cube, so that cube must be inside the lifted region — the declared initial states themselves do not change. The CEGAR loop sets the option; the KMTS-exploration API (`predicate-cube`) and the verify orchestrator do not (they keep the full lift).

Unreached cells exist as states with no outgoing edge. The loop already masks edgeless cells to ⊥ and excludes them from convergence (the unsatisfiable-cell rule), so they ride that path — but they are **not** reported as ⊥: `CegarTrace::unlifted_cells` names them, `tally_cells` excludes them, and the Track I.1 `violating_cells` / `undecided_cells` samples skip them.

Measured with the reset pinned: 92 of 256 cells reachable on the i2c lift; 10.9 → 2.9 s. With a free reset every cell is a root and nothing is skipped (see the correction above).

## What to update, per consumer

### Anyone parsing `btor2 cegar --json` or the API `cegar` response

- `verdict` has a fourth count, **`unlifted_cells`** (`#[serde(default)]`, so older clients deserialise; the CLI JSON always prints it). `true_cells + false_cells + unknown_cells + unlifted_cells` = the cube count.
- `true/false/unknown_cells` are now over the cells the lift decided. A gate on `false_cells == 0` or `unknown_cells == 0` keeps its meaning; a gate on `false_cells == cube_count` (or any absolute count) does not — it was counting unreached cells.
- `violating_cells` / `undecided_cells` (capped samples) list reached cells only, so the sample can differ from before even when the verdict is the same.
- The initial-state verdict, `counterexample`, `terminated_with`, `iterations` and the per-iteration records are unchanged in meaning. Per-iteration `verdict` summaries carry the same fourth count.
- **`unlifted_cells` is never "needs refinement".** On a converged run with a definite initial verdict, `unknown_cells` is 0 even though most of the cube may be unlifted; before this change a converged run with unsatisfiable cells could already print "INDEFINITE" for them — that wart is gone too.

### monono / ROSF

- No wire-shape change on `sv verify-auto` (symbolic engine; its summaries carry `unlifted_cells: 0`). Expect faster `btor2 cegar` lanes and slightly faster z3 model extraction everywhere.
- The explicit lift's **initial cube is still `cube_0` unless `--config-value` pins are given** — a pre-existing placeholder, now filed as [mununu#609](https://github.com/Mumunu-team/mununu/issues/609). A4 does not change it: the verdict is identical at whichever initial cells the lift has. With #609 fixed, the reachable set (and the counts) will follow the real reset cell.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | `btor2 cegar` report shape (`unlifted_cells`, counts over decided cells); lift wall | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The mechanism: reached cells keep their edges, unreached cells are reported; the CEGAR
#    tally excludes them and the initial verdict matches the full lift.
cargo test -p mununu-core --lib -- a4_ p1_postimage_lift_matches_all_pairs_lift

# 2. Your own design: the reachable fraction, from the lift's debug line.
RUST_LOG=mununu_core::adapter::btor2::kmts_lift=debug mununu --quiet btor2 cegar design.btor2 \
  --formula '…' --predicate … --engine explicit --json 2>&1 >/dev/null | grep reachability

# 3. Diff a corpus's `btor2 cegar --json`: `outcome` polarity and `counterexample` must match;
#    `verdict.false_cells` falls, `verdict.unlifted_cells` appears. A polarity change is NOT
#    this change and is worth reporting with the design.
```

## Not covered here

- The all-pairs seam (`may_postimage` off, |P| < 2, or the lift's `NotApplicable` fallback) and the lazy lift still lift every cell.
- mununu#609 (the cube_0 initial state) — filed, open.
- M6 (a coarse-may skip list across CEGAR iterations) — next on the roadmap; its measurement starts from these numbers.
