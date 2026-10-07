# Consumer briefing — 2026-10 the explicit cube lift's may-relation comes from the exact engine's BDDs; a literal the register cannot hold is now `false`, not a different atom

> **Audience:** monono, ROSF, and any consumer of `btor2 cegar --engine explicit` (CLI JSON or `POST /api/v1/btor2/cegar`), `btor2 verify-recoverability` / `sv verify-recoverability` on the scalable (cube) ladder, or `@mununu_predicate` / `--predicate` atoms on the explicit lift.
>
> **Provenance:** Roadmap 2 (dependency levers) of the engine-performance roadmap, step 5, with the latent inconsistency it surfaced. Measured on the i2c lift at |P| = 8 and 10 (`scripts/profile_cases.py`, case `cube-rtl-i2c`). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

Two things, one PR. The explicit cube lift's **post-image may-relation** is now read off the exact engine's abstract relation (one BDD over `2|P|` predicate variables) instead of one z3 all-SAT loop per source cube — the lift is **4–7× faster** on the i2c reference with **byte-identical output** in every measured arm, and the z3 loop remains the fallback for any cone the bit-blaster refuses. Landing it exposed and fixed a pre-existing inconsistency: a `reg == v` literal that the register **cannot hold** (`ctrl == 2` on a 1-bit `ctrl`) was silently **masked** by the SMT may/must lowerings into a different atom (`ctrl == 0`), while the simulator, the reset cube and the exact engine read it as never true. There is one reading now — never true — on every path.

| `btor2 cegar --engine explicit`, i2c lift | SMT loop (before) | BDD backend (after) |
|---|---|---|
| 8 predicates, 1 post-image worker | 8.58 s | **1.73 s** |
| 8 predicates, 8 workers (the default) | 5.22 s | **1.84 s** (CPU −88 %) |
| 10 predicates, 1 worker | 50.4 s | **7.6 s** |
| 10 predicates, 8 workers | 35.1 s | **7.5 s** (CPU −91 %) |
| `outcome`, `verdict` counts, `counterexample`, `terminated_with`, `iterations` | — | identical (md5 of the stripped JSON) |

## What changed

### The BDD post-image backend (`PostimageBackend::Bdd`, the default)

The `symbolic` engine has always built `R_may(p, p') = ∃(x ∪ i). A(x,p) ∧ A'(x,i,p')` — the abstract may-relation over predicate cubes — as one BDD, validated against a brute-force concrete sweep. The explicit lift now reads its may-map off that BDD: the predicates' cone is bit-blasted (`cone_leaf_nids` on the predicate registers, the same cone the exact engine's own cube path uses), the relation is built once, and each source cube's targets are read by an output-sensitive split (`AbstractRelation::may_targets`). The same worklist BFS as before runs on top (A4's reachable-only roots, or every cell), so `unlifted_cells` is unchanged in meaning.

The backend answers only when it can; otherwise the z3 loop runs exactly as before. It declines: a predicate not over a `state` cell, a cone over the bit cap, a design with memories, a node-budget or arena refusal, an expired run budget. Neither backend models `constraint` in the may-relation, so the two agree on `constraint` designs as well.

`MUNUNU_CUBE_POSTIMAGE_BACKEND` = `bdd` (default) | `smt` (the previous loop) | `check` (both; every cell where they differ is logged at `warn`, the SMT map is the answer — the differential-oracle mode). `MUNUNU_CUBE_POSTIMAGE_THREADS` only matters on the `smt` path.

At |P| = 10 the backend builds the relation in 0.56 s and reads 1,024 cells in 0.13 s; what remains of the lift (~6.9 of 7.6 s) is the hyper-must pass — the wall now, and the next candidate.

### One reading of `register <op> literal` on every path (the fix)

`predicate_expr::cmp_constraint` (the may side) and `smt_must_edge::build_predicate_constraint` (the must side) masked a literal to the register's bit-width: `ctrl == 2` on a 1-bit `ctrl` became `ctrl == 0`, an atom true on half the states. `PredicateExpr::eval` (the simulator and the reset cube), `match_widths` (register-vs-register atoms) and the exact engine's `predicate_bdd` compare with the register zero-extended, where `ctrl == 2` is never true. Both readings coexisted inside one lift; they met when a BDD may-map was combined with an SMT must pass, and a 48-bit arithmetic-relational test went `Holds` → `⊥`. The `check` mode localised it to the one atom.

Now `literal_cmp` is the one SMT lowering — comparison at `max(width, 64)` bits with the register zero-extended — and the recoverability ladder's auto-seed no longer spells a constant the register cannot hold (that is how `ctrl == 2` arose: the 48-bit addend `2` of `data == target + 2` sits in `ctrl`'s next cone).

## What to update, per consumer

### Anyone passing `@mununu_predicate` / `--predicate` atoms to the explicit lift

- **An out-of-range literal changes meaning.** `reg == v` with `v ≥ 2^width` used to behave as `reg == (v mod 2^width)` on the SMT paths; it is now an always-false atom (an infeasible cell, never reached), and `reg != v` always true, `reg < v` always true, `reg > v` always false. This is the reading the simulator and the exact engine always had. A spec that relied on the old aliasing was relying on a silent rewrite of its own atom; the atom it meant can be written directly. No warning is emitted yet (tracked below).
- In-range literals are unchanged. Register-vs-register and `reg == reg + k` atoms are unchanged (they already zero-extended).

### Anyone parsing `btor2 cegar --json` or the API `cegar` response

- No wire-shape change. For well-formed atoms the output is identical (measured md5-identical on the reference lift at |P| = 8 and 10, with and without reachable-only roots).
- Expect the lift to be several times faster; a lane `timeout` sized to the old lift has headroom now, not less.

### `verify-recoverability` on the scalable (cube) ladder

- The auto-seed may produce **fewer** predicates on designs whose control register's cone carries a wider datapath constant (the skipped atoms were always-false or aliases). A verdict can only move ⊥ → definite from that, never a definite the other way (monotone refinement). None moved in the test corpus.

### monono / ROSF

- No wire-shape change on `sv verify-auto` (symbolic engine, unaffected). `sv verify-recoverability` lanes that reach the cube ladder get the faster lift and the literal fix above.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | explicit-lift speed; the literal-width semantics on the SMT paths | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The identity between the two backends, cell for cell, on four fixtures (incl. i2c and the
#    out-of-range literal), and the literal lowering's contract.
cargo test -p mununu-core --lib -- r2_step5_bdd_postimage literal_cmp_does_not_mask

# 2. Your own design: run both backends and let the engine report any differing cell.
RUST_LOG=mununu_core::adapter::btor2::kmts_lift=warn MUNUNU_CUBE_POSTIMAGE_BACKEND=check \
  mununu --quiet btor2 cegar design.btor2 --formula '…' --predicate … --engine explicit --json \
  2>&1 >/dev/null | grep -E "DIFFER|cross-check"

# 3. Diff a corpus's `btor2 cegar --json` between the old binary and this one: identical for
#    well-formed atoms. A difference on a design with no out-of-range literal is worth reporting
#    with the design — `check` mode names the cell.
```

## Not covered here

- A **warning** when a `--predicate` / `@mununu_predicate` literal does not fit its register — the lift accepts the atom silently (as an always-false cell). Worth adding; not in this PR.
- The hyper-must pass, now the lift's wall (~90 % at |P| = 10), stays on z3. A symbolic must relation exists in the exact engine (`abstract_relation` with a `MustSemantics`) but computes per-target must edges, not the hyper-must sets the explicit lift uses; bridging them is the next candidate and is not started.
- The lazy lift and the all-pairs seam (`may_postimage` off, |P| < 2) still use the SMT path.
