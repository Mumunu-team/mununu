# Consumer briefing — 2026-10 the SmtAllPairs hyper-must composition promotes the ∀∃ singletons first (mununu#667)

> **Audience:** `btor2 verify-recoverability` callers (its ladder runs `smt-hyper-must`), every `sv verify-auto` lane whose ⊥ re-plan reaches the cube rescue, and anyone passing `--must-edge-inference smt-hyper-must` to `btor2 cegar`: `AG EF` rows that stayed ⊥ under hyper-must while the per-target form decided them now decide.
>
> **Related:** [`2026-10-exact-inputs-in-the-state.md`](2026-10-exact-inputs-in-the-state.md) (the arbiter row's lift), `docs/design/kmts-theory.md` §7.4. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change, ⊥ → decided only.** On the SmtAllPairs / compound cube path the `SmtHyperMust` arm emitted one edge per source — `src →□ may_succ(src)`, the whole may-successor set. The inner μ of `AG EF good` can use such an edge only once *every* may-successor is already in its approximant, so a source that reaches `good` under some input but may also stay never fed the fixpoint. The arm now promotes the ∀∃ singletons (`src →□ {t}`) to Sharp first — as the sampling path always did — and emits the full-set edge only for sources no singleton promoted. Nothing that decided before decides differently; a singleton is a hyper-must edge with |T| = 1, a proved obligation like any other.

## What changed

- [`kmts_lift.rs`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs), the `MustEdgeInference::SmtHyperMust` arm of the SmtAllPairs composition: `must_edges_over(&may_edges)` first (Sharp), then the precomputed full-set hyper edge for un-promoted sources with ≥ 2 may-successors. The adapter warning reports both counts (`promoted N ∀∃ singleton edge(s) to Sharp and emitted M MustHyperOnly edge(s)`).
- Routes affected: the session-cache route (`may_and_hyper_must_edges`, taken at |P| = 1 — the arbiter row) and the post-image route (`hyper_must_from_exact_may`); the sampling path's arm was already singleton-first and is unchanged.

## Measured

| | result |
|---|---|
| vendored `prim_arbiter_tree_n8` lift, `AG EF (gnt_o == 0)`, one predicate, `cegar_refine_loop` (SmtAllPairs may, `LiftStrategy::Eager`) | `Off`: ⊥ · `smt-per-target-standard`: HOLDS · **`smt-hyper-must`: ⊥ → HOLDS** (`x663_the_cube_decides_the_arbiter_recoverability_with_per_target_must`, which now runs the hyper-must arm too) |
| wall-class matrix (`mununu-sva`, the worktree) | **2 passed / 0 failed in 37 s**; every row's verdict as before — soundness invariant intact (no lever contradicts an oracle), no row moved (the set has no hyper-must-only row; the arbiter is the class representative pending an RTL-backed matrix row) |
| `cargo clippy -D warnings` on `mununu-core` (`--features api`), `x663_` tests | clean; 2/2 |

## What to update, per consumer

- **`btor2 verify-recoverability` lanes / the ⊥ re-plan:** expect some `unknown` → `holds` / `violated` on `AG EF` targets whose source cells have both a reaching input and a staying one (arbiters, FIFOs with a drain input). No decided verdict changes.
- **Report parsers:** no shape changes; the `[B.2 may+hyper-must]` warning text now carries two counts.
- **mununu#663's `auto` mapping** (Liveness / Mixed → `smt-per-target-standard`) stands; the measurement that motivated it is no longer a difference in reach between the two forms on these rows.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | cube verdicts under hyper-must | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x663_the_cube_decides_the_arbiter_recoverability_with_per_target_must
```

## Not covered here

- The full-set hyper edge still uses the whole may-successor set as `T` (not minimised); a source whose singletons all fail and whose set edge needs a strict subset of `T` stays ⊥.
- `prim_fifo_sync` `depth_o == 0` (the issue's second row) is not vendored as a fixture; it is expected to move by the same mechanism and is not re-measured here.
