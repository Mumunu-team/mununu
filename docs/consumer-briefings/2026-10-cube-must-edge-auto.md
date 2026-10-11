# Consumer briefing — 2026-10 `sv verify-auto`: the cube member's must-edge policy is `auto` — per-target must for liveness-shaped properties, may-only for the rest

> **Audience:** anyone running `sv verify-auto` on recoverability (`AG EF`) or response (`AG AF`) properties with the default engine or `--engine explicit`, and anyone who passes `--must-edge-inference` / `must_edge_inference` explicitly.
>
> **Related:** closes [mununu#663](https://github.com/Mumunu-team/mununu/issues/663); depends on the hyper-must fix [#664](https://github.com/Mumunu-team/mununu/issues/664). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change (a ⊥ becomes HOLDS or VIOLATED on liveness-shaped properties) + a new default value for an existing flag; no wire-shape change.** The 3-valued `◇` turns true only through **must** edges, and the cube member's default emitted none — so on any `AG EF` / `AG AF` property the explicit engine was *structurally* ⊥: it could never return HOLDS and fell to the ⊥ re-plan (a second lift). The default is now `auto`: a Liveness/Mixed-class formula (a μ under a ν) runs its cube member with `smt-per-target-standard` (Z3 ∀ state ∃ input, per target — the form that decides the measured rows; `smt-hyper-must` leaves them ⊥, [mununu#667](https://github.com/Mumunu-team/mununu/issues/667)) and safety / reachability / propositional formulas stay may-only. An explicit policy is honoured as given; `off` now also means "no auto resolution".

## What changed

- [`effective_must_edge_inference`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs): per property, `Off` + `must_edge_auto` resolves by `Formula::property_class()` — `Liveness | Mixed → SmtPerTargetStandard`, else `Off`. Explicit `smt-per-target`, `smt-per-target-standard`, `smt-hyper-must` apply to every property as before.
- **CLI:** `--must-edge-inference auto` is the new default of `sv verify-auto` and `sv mutate` (which runs verify-auto on both sides of the mutation); `off` is still accepted and switches the resolution off. On `btor2 cegar` and `sv cegar` `auto` means `off` (unchanged behaviour).
- **API:** `sv/verify-auto` and `sv/mutate`: `must_edge_inference` absent or `"auto"` → auto; `"off"` → explicit may-only. `btor2/cegar`: `"auto"` accepted, means `"off"`. Request schema regenerated (field description).
- **Report:** the `abstraction-posture` note names the policy as `auto — SMT ∀∃ per-target must (smt-per-target-standard) for Liveness/Mixed-class properties (a μ under a ν), may-only for the rest` when auto applied.

## Measured

All rows 2026-10-11, `sv verify-auto --engine explicit --preprocess-sv2v`, OpenTitan corpus designs, one `@mununu_guarantee` each; "cube alone" = `MUNUNU_NO_INPUT_STATE=1` so the ⊥ re-plan's exact pass cannot mask the cube, main-phase outcome from the `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` breadcrumb.

| design · property | `off` (old default), cube main pass | `auto` = `smt-per-target-standard`, cube main pass | `smt-hyper-must`, cube main pass | final verdict, default run (either policy) |
|---|---|---|---|---|
| `prim_arbiter_tree` · `AG EF (gnt_o == 0)` | ⊥ (87 ms) | **HOLDS** (123 ms) | ⊥ ([#667](https://github.com/Mumunu-team/mununu/issues/667)) | HOLDS — by the ⊥ re-plan's exact pass (input-state product), 90–130 ms |
| `prim_fifo_sync` · `AG EF (depth_o == 0)` | ⊥ (42 ms) | **HOLDS** (64 ms) | ⊥ | HOLDS — by the re-plan's exact pass, 30–40 ms |
| `prim_packer_fifo` · `AG EF (depth_o == 0)` | ⊥ (51 ms) | ⊥ (44 ms) | ⊥ (45 ms) | HOLDS — by the re-plan's exact pass, 20–35 ms |
| `prim_arbiter_tree` · one-hot safety (control) | HOLDS (183–188 ms) | HOLDS (178–216 ms; `auto` → may-only on a Safety-class formula) | — | — |

So on this corpus the FINAL verdicts do not change (the ⊥ re-plan's exact pass already decided every row); what changes is that the cube member decides 2 of 3 `AG EF` rows itself, and where the exact engine cannot run (bit cap, memories, the input-state product switched off), the cube's `auto` verdict is the one a lane gets — `prim_arbiter_tree` with the product off (`MUNUNU_NO_INPUT_STATE=1`; the per-request `--no-input-state` reaches the re-plan from the follow-up PR to #659) went from ⊥ to HOLDS. Cost of the per-target queries on these designs: +20–40 ms per property. The cube's own ⊥ on `prim_packer_fifo` is a predicate-coverage limit, not a policy one.

## What to update, per consumer

- **monono / rosf lanes with `AG EF` / `AG AF` properties under `--engine explicit`:** a property that used to read `unknown` (⊥) can now read HOLDS or VIOLATED from the cube member directly; a lane that pinned those ⊥ by name should unpin them. Expect a longer cube pass on those properties (the ∀∃ queries) and a shorter total when the ⊥ re-plan no longer runs.
- **Lanes that pass `--must-edge-inference off` deliberately** (may-only soundness posture): unchanged — `off` still means may-only, and now also disables the auto resolution.
- **Report parsers:** no shape change; the posture note's text changed as above.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the cube member's default policy | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x663_
mununu sv verify-auto <design>.sv --top <top> --engine explicit                         # auto
mununu sv verify-auto <design>.sv --top <top> --engine explicit --must-edge-inference off  # the old default
```

## Not covered here

- The exact engine remains the first member of the default portfolio and decides most of these properties before the cube runs; `auto` matters where the exact engine abstains (bit cap, memories).
- `btor2 cegar` keeps `off` as its effective default; a per-formula auto there is a separate change.
