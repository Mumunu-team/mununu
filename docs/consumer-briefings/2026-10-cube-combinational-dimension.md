# Consumer briefing — 2026-10 explicit engine: a predicate over a combinational signal is its own cube dimension, never the nearest state cell

> **Audience:** anyone running the explicit (predicate-cube) engine — `sv verify-auto --engine explicit`, the default portfolio's explicit member, `btor2 cegar` — on properties whose atoms read an `assign`ed output or any combinational function of state; monono's formal lane first (it held its engine at `bd5dd63` on this).
>
> **Related:** closes [mununu#637](https://github.com/Mumunu-team/mununu/issues/637). Fixture: [`examples/verify/v14_pulse_cdc_sync/`](../../examples/verify/v14_pulse_cdc_sync/). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change (a spurious VIOLATED becomes HOLDS; a ⊥ at an uninhabited initial cell becomes a verdict) + one new refusal; no wire-shape change.** The cube lift bound a combinational signal named in a predicate (`pulse_out = sync_q ^ sync_d_q`, `ibus_ack = (st_q == S_ACK) && !sel_dbus_q`) to the *nearest state cell in its cone*, so `pulse_out == 1` was verified as `sync_q == 1` — a different predicate — and a cell no state inhabits refuted a tautology. The signal is now its own dimension, with its reset truth observed from the init valuation, and the initial cube set is filtered to the cells a concrete reset state inhabits (the product over the dimensions admitted `{pulse_out == 0, sync_q != sync_d_q}` once `sync_q` is free at reset — edgeless, masked to ⊥). The `engine-contradiction` ⊥ on `pulse_cdc_sva_sva_0` and `wb_mem_client_sva_sva_0` becomes a plain HOLDS from every engine.

## What changed

- [`resolve_predicate_registers`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs): a predicate's register name resolves only in value-preserving ways — a value-identical alias of a state cell (`uext … 0 NAME`, the async-reset mux, a mirroring port) becomes the cell's canonical symbol; a combinational signal's own symbol is kept as the dimension's source. The loose "nearest state in the cone" walk is gone from the lift. (The same loose walk was removed from `check-fsm` and `sv mutate` in #643.)
- The cube's **initial cells** read a combinational dimension at its reset value (the signal simulated at the init valuation) when its cone bottoms out in `init`-pinned registers; an input in the cone leaves it free, as before.
- [`cycle_zero_feasible_cubes`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs): of the candidate initial cubes — a product over the dimensions, exact only while they are independent — only those a concrete cycle-0 state inhabits are initial. One quantifier-free Z3 query per candidate (the predicates at their cube polarities, plus the `init` / config pins), asked only when the dimensions are coupled (a combinational name, a compound, two atoms over one register); a cube is dropped on a *proven* Unsat only, and a filter that would drop every candidate keeps the product. Applies to the lift's initial states (`btor2 cegar`, the CEGAR verbs) and to `verify-auto`'s init-cube read alike. A property that used to read ⊥ at such a cell (`Unknown` with a small `unknown_cells`) now decides.
- The sampling may-inference (`--may-edge-inference off`, the lift's default outside `verify-auto`) now **refuses** a combinational dimension by name (`predicate … is over the combinational signal … use may_edge_inference = SmtAllPairs`) instead of silently reading it as 0 on every step. `verify-auto` and the CEGAR verbs already run the uniform SMT image, so they are unaffected by the refusal.

## Measured

| design · property | before | after |
|---|---|---|
| `pulse_cdc` · `a_out_is_the_edge` (`pulse_out == 1 \|-> sync_q != sync_d_q`), `--engine explicit` | **VIOLATED (1 cell)** — the cell `{pulse_out==1, sync_q==sync_d_q}`; portfolio: `engine-contradiction` ⊥ | HOLDS (with the resolution fix alone: `Unknown { unknown_cells: 2 }`, the uninhabited initial cell) |
| `wb_mem_client` · `a_ack_exclusive` (`!(ibus_ack && dbus_ack)`), `--engine explicit` | **VIOLATED (1 cell)**; portfolio: `engine-contradiction` ⊥ | HOLDS (same mechanism; measured on the consumer's tree, not vendored) |
| seam fixture `out = a ^ b` (one register named only by its alias) | `out` bound to `a` | `out` kept; no edge reaches `{out==1, a==b}`; `out == 1` false at reset |
| the same with `a` free at reset | initial cells include `{out==1, a==0, b==0}` and `{out==0, a==1, b==0}` | initial = exactly the two cells a reset state inhabits |

Identical before/after under the `bdd`, `smt` and `check` post-image backends: the defect predates #616.

## What to update, per consumer

- **monono:** unpin `pulse_cdc_sva_sva_0` and `wb_mem_client_sva_sva_0` in `verify.sh` (they were pinned ⊥ by name) and let the engine move off `bd5dd63` together with the oxidd-0.13 bump (`2026-10-oxidd-0-13-stale-local-store.md`).
- **Lanes using `btor2 cegar` with the default (sampling) may-inference on a predicate over a combinational signal:** expect the new refusal; pass `--may-edge-inference smt-all-pairs` (what `verify-auto` does).
- **Report parsers:** nothing changed.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the cube lift's seeding + init cells | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo test -p mununu-core --lib --features api -- x637_                    # the seam fixture, the sampling refusal
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_637_
```

## Not covered here

- The SMT post-image worker still resolves state names only; a combinational dimension makes it fall back to the uniform all-pairs image (correct, slower). Teaching the worker the primed signal cache is a performance follow-up.
- `wb_mem_client` is measured, not vendored; `pulse_cdc` carries the e2e.
