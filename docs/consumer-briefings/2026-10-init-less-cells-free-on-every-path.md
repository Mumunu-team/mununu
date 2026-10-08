# Consumer briefing — 2026-10 an init-less register is free at cycle 0 on every path; the explicit lift starts from the `init` lines, not cube_0

> **Audience:** monono first — this is the convergence [mununu#579](https://github.com/Mumunu-team/mununu/issues/579) asked for after your [#577](https://github.com/Mumunu-team/mununu/issues/577), and it moves verdicts on the `sv verify-auto` cube path. Also ROSF, and any consumer of `btor2 cegar --engine explicit` (CLI JSON or `POST /api/v1/btor2/cegar` / `predicate-cube`) without `--config-value` pins.
>
> **Related:** closes [mununu#609](https://github.com/Mumunu-team/mununu/issues/609) (the explicit lift's initial state); the init-convention half of [mununu#579](https://github.com/Mumunu-team/mununu/issues/579) (its report self-check half shipped in #583 as `report-self-contradiction`). Builds on [#578](https://github.com/Mumunu-team/mununu/issues/578)'s withheld VIOLATED (briefing [`2026-09-withheld-violated-unestablished-init.md`](2026-09-withheld-violated-unestablished-init.md)). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change, two paths.** (1) On `sv verify-auto`'s predicate-cube path a state cell with **no BTOR2 `init` line** was silently pinned to **0** at cycle 0, while the exact engine and the reachability portfolio left the same cell **free** — one portfolio run, two initial states, which is the mechanism behind #577's pair of mutually-wrong verdicts. The cube path now leaves it free too, so every engine reads cycle 0 the same way. (2) The explicit predicate-cube lift (`btor2 cegar --engine explicit`, the API `cegar` handlers) started from **cube_0 — every predicate false** — unless you passed `--config-value`; it now starts from the set of cubes consistent with the design's `init` lines. Any verdict you hold from either path **on a design with an init-less register in the property's cone, or from `btor2 cegar` without config pins**, was read at a start state the design does not have — re-run it. A new named ⊥, `unestablished-initial-cubes`, bounds the enumeration.

## Which of your verdicts move

### `sv verify-auto` (cube path)

The suspect set is mechanical. A property moves only if **all** of:

- the lifted BTOR2 has a state cell with **no `init` line** in the property's cone. With reset-gating ON (the default) that is a register the reset does **not** establish — one with no reset branch, one reset to a non-constant, one behind a blackbox — because `inject_reset_init` writes an `init` for every register whose reset value it can recover, and `inject_zero_init` completes a reset-less design to the `setundef -zero` power-up. With `--no-gate-reset`, it is **every** register the reset would have established (no reset is applied, nothing is injected);
- **and** the property's predicates mention that register (directly, or through a compound atom);
- **and** the verdict came from the cube engine (`decided_by` names it, or the run was `--engine symbolic`).

What happens to such a property:

| before (cell pinned to 0) | now (cell free at cycle 0) | why |
|---|---|---|
| `HOLDS` | `HOLDS`, or `VIOLATED`, or `unknown` (`unestablished-initial-state`) | the old HOLDS was read from fewer start states than the model has — **it was the unsound direction**. If the property fails from a start the pin excluded, it is VIOLATED on the model; under a **pinned** reset the #578 pass withholds that as `unestablished-initial-state` (the reset pin is what removed the reset path), un-gated it stands, with a cycle-0 witness. |
| `VIOLATED` | `VIOLATED` (unchanged) | more start states cannot hide a refutation that held from 0. |
| `unknown` | `unknown`, or **`unknown` with `bottom_reason.kind == "unestablished-initial-cubes"`** | the cube's initial set is the product of the free dimensions; past 16 it abstains by name instead of enumerating `2^n`. |

**A HOLDS that becomes VIOLATED is not a regression in the engine; it is a start state your model had and the cube was not reading.** The remedy is the one #578's briefing gave: establish the value — apply the reset (gating ON), add an `init` through the sidecar, or `--config-value` the register. The measured instance in-tree: the un-gated `disable iff (!rst_n) state != 3` on a 2-bit FSM with no `init` moves HOLDS → VIOLATED, because `state == 3` with `rst_n` high is an admissible first cycle once nothing resets `state`; gated, it HOLDS as before (`init state = 0` is injected from the reset mux).

```bash
# Properties on the cube path whose cone touches an init-less cell, after the change:
mununu --quiet sv verify-auto design.sv --json \
  | jq '[.properties[] | select(.bottom_reason.kind == "unestablished-initial-state"
                             or .bottom_reason.kind == "unestablished-initial-cubes")
         | {property: .name, kind: .bottom_reason.kind, registers: .bottom_reason.registers}]'
# How many registers the lift left init-less (the size of the remedy):
MUNUNU_SHADOW_BTOR2_DUMP=/tmp/m.btor2 mununu --quiet sv verify-auto design.sv >/dev/null
diff <(grep -c '^[0-9]* state ' /tmp/m.btor2) <(grep -c '^[0-9]* init ' /tmp/m.btor2)
```

### `btor2 cegar --engine explicit` / API `cegar`, `predicate-cube` — without config pins

Every verdict was read from **cube_0** (all predicates false). On the i2c reference lift that cube is not the reset cell (`c_state == 0` is *true* at reset), and 72 of 256 cells were "reachable" from a state no concrete reset inhabits. Now:

- with `--config-value` / sidecar `config_values` (the verify orchestrator always passes them): **unchanged** — the R-S8 admissible cubes were already the initial set;
- without: the initial cubes are those **consistent with the `init` lines** — one cube when every predicate register has an `init`, several when some are init-less (each unconstrained bit doubles the set), and cube_0 only as the last-resort fallback when no cube is consistent (which cannot happen on a parsed model; it is kept so the lift never has zero initial states).

A `reachable`/`EF` that was VIOLATED at cube_0 can become HOLDS; an `AG` that HOLDS at cube_0 can become VIOLATED. Both are the lift reading the design's start instead of a conventional one. The `--json` `initial_states` count and the reachability debug line (`RUST_LOG=mununu_core::adapter::btor2::kmts_lift=debug`) show the new set.

## What changed, precisely

- `state_cell_init_values` (`adapter/slang/verify_auto.rs`) returns **only** cells that have an `init` line; it used to `unwrap_or(0)`, which made "init 0" and "no init" indistinguishable one line after the only place that could tell them apart.
- A predicate over an init-less register is a **free initial dimension** of the cube — the same mechanism H.B uses for a free input — and the initial cube set is the product of the free dimensions, filtered by the #503 exclusivity rule (two values of one register never co-occur).
- **Established-or-not is decided per register through the strict alias resolver** (`resolve_state_alias`: `uext`-0 renames and, under a pinned reset, the async-reset mux), not by the name the lift happened to give the cell. yosys emits many state cells without a symbol, and the loose naming pass can call a counter's cell `cnt_d` while every predicate says `cnt_q`; the old `unwrap_or(0)` hid that, and the first version of this change read such a register as free and refuted a true invariant at cycle 0 (caught by `e2e_counter_bound_flips_saturating_monotonicity`). A combinational-of-state name is observed at the reset state only when every state cell in its cone has an `init` and no primary input feeds it; otherwise it is free too.
- `MAX_FREE_INIT_DIMENSIONS = 16`. Past it the property abstains with `BottomReason::UnestablishedInitialCubes { free_dimensions, cap, registers }` — `kind: "unestablished-initial-cubes"`, `determinism: reproducible`, and `registers` (the count of init-less registers involved, the remedy's size) on the API view. The first convergence attempt freed every init-less bit unbounded and overflowed the stack on real RTL; the bound is the fix for that.
- `kmts_lift::initial_cube_indices` evaluates each predicate on the `init` valuation (`reset_truth_per_bit`) and keeps every cube consistent with it (`cubes_consistent_with`); config values take precedence (R-S8), as before. The lazy lift (`materialize_clts_from_lazy`) reads the same set.
- `reset_init.rs`'s header table — the audit that documented the three-way split — now records the converged convention and names the tests that assert it.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | verdicts on the cube path and the explicit lift; new bottom reason | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e expectation moved | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The converged convention, asserted (cube path + explicit lift + the cap):
cargo test -p mununu-core --lib --features api -- \
  an_initless_cell_is_free_at_cycle_zero x609_initial_cubes_come_from_the_init_lines \
  the_free_initial_dimension_cap_abstains_by_name

# 2. The measured RTL instance, in the mununu-sva image (needs slang + yosys + z3):
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored \
  e2e_reset_handled_by_gating_or_as_free_input

# 3. Your corpus: diff `sv verify-auto --json` between the old binary and this one. Every
#    property that moved should have an init-less register in its cone (step 1's jq above) —
#    one that does not is worth reporting with the design.
```

## Not covered here

- The `symbolic` engine's own init cube (`symbolic_engine.rs`) was already free for init-less cells; nothing changed there.
- `unestablished-initial-cubes` is a cap, not a decision procedure: a property over more than 16 free initial bits abstains. A symbolic initial-cube set (one BDD over the predicate variables, read off the `init` constraint the way the post-image is read off the exact relation since #616) would remove the cap; not in this PR.
- #579's broader bound/witness self-check rule is **not** implemented, per its own retraction (the rule is unsound unless the witnesses exhaust the range). `AG EF(p)` HOLDS beside `EF(p)` VIOLATED is #599, its own PR.
