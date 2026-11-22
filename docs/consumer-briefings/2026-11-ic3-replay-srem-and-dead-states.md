# Consumer briefing — the owned IC3 ships two classes of counterexample it used to drop (mununu#1055, mununu#1056)

> **Audience:** maintainers of the repositories that read `mununu btor2 verify --owned-only`, `sv verify-auto`'s reach rescue on a host without rIC3, or `POST /api/v1/btor2/verify` (rosf, monono, mununu-ui). Not a release note.
>
> Source of truth: [`Ic3::concretize`](../../crates/mununu-core/src/adapter/btor2/ic3.rs#L1095) · [`eval_op`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L6749) · [`Bv::sdiv`](../../crates/mununu-core/src/adapter/btor2/bv.rs#L330) — surface: CLI+API

## TL;DR

Two rows of the HWMCC'20 head-to-head (owned `ic3` alone against rIC3 `-e ic3`, 2026-11-17) had the owned IC3 find a real counterexample and then abstain. Both paths now ship the counterexample; no flag, field, route or default changes.

1. **The concrete simulator evaluates the division family and the overflow detectors** (`udiv`, `urem`, `sdiv`, `srem`, `smod`, `uaddo`, `usubo`, `saddo`, `ssubo`, `umulo`, `smulo`, `sdivo`), with the circuit's conventions (SMT-LIB's total extensions for a zero divisor). Before, a trace through any of them failed to replay and the owned IC3 abstained with `operator Srem unsupported in Phase 1 bit-blaster` (HWMCC'20 `anderson.3`, a counterexample at frame 3 in 0.3 s). Every member that replays on the simulator before shipping `violated` (`sat-bmc`, `ic3`, the witness carriers, native BMC's trace) gains the same operators; a trace through `read` / `write` (arrays) still does not replay, and the engines refuse array designs before a trace exists.
2. **A counterexample chain whose cube keeps a dead state still concretises.** With input constraints a lifted cube is a may set: it can keep a state whose constraint-satisfying inputs never reach the next cube, and the step-by-step concretisation found no model and abstained (`the counterexample chain did not concretise at step 10 of 11`, HWMCC'20 `shift_register_top_w16_d8_e0`: pop forbidden when empty, push when full). Now the dead state is blocked as a successor of the previous step and that step is re-solved, backing up as far as needed; a dry step 0, or 4,096 blocked states, still abstains with the count in the reason. Every shipped step is one SAT model of the transition relation under the constraints, and the trace is replayed before it ships.

What changes for a consumer: a `--owned-only` row (or a reach-rescue row on a host without rIC3) that was `unknown` with one of the two reasons above can now be `violated`, with `reachable_by: ["ic3"]` and a trace. In the full portfolio beside rIC3 nothing changes: another member decided these rows. The `ic3 search` debug line (`RUST_LOG=mununu_core::adapter::btor2::ic3=debug`) gains `concretize_retries=<n>`; a log scraper keyed on that line's fields must allow the new one.

## Per consumer

### rosf · monono (CLI and HTTP)

- `--owned-only` / `owned_only: true`: more `violated` rows on designs with signed division or with input constraints that leave dead states in the owned IC3's cubes; `reachable_by` reads `"ic3"` there. Shape unchanged.
- `sv verify-auto` on a host without rIC3: the reach rescue's `ic3` member decides such rows inside its slice; `decided_by` reads `reach-rescue` as before.
- Full portfolio beside rIC3: no change.

### mununu-ui

No CTXDSL surface; no change required.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the owned member's reach under `--owned-only`; the simulator's operators |
| `mununu-sva` (dev / e2e) | yes | same |
| consumers' own images | no | no schema change; `docs/api-schemas/` untouched |

## Provenance

Found by the laptop's head-to-head on the M2 68 (private `experiments/competitiveness-2026-11/h2h-ic3-2026-11-17/`, main `3d1c235a`): 2 rows where the owned IC3 found a real counterexample and dropped it, rIC3 1.5.2 `-e ic3` SAT on both. Reproducer in-repo for the first change: `a_counterexample_through_srem_replays_and_ships` (a 4-bit `srem` whose remainder reaches −1; on the parent the engine stops with the `Srem unsupported` reason, in the image with only the test module spliced onto the parent's file), with `the_division_family_and_overflow_detectors_match_integer_semantics_at_four_bits` (every 4-bit pair against integer arithmetic) as the semantics pin; `anderson.3` through the head-to-head's one-file driver: parent `stopped` (0.25 s), this change `violated depth=4` (0.26 s). The second change has **no failing reproducer**: `shift_register_top_w16_d8_e0` is `violated depth=16` on the current parent too (the head-to-head ran on `3d1c235a`; N5.6, `fc85700f`, changed the member's search trajectory and the current chain concretises), and the unit design `a_chain_through_a_dead_state_concretises_by_backing_up` passes on the parent in both polarities (3 of 3 runs: the solver's step-0 model lands in the alive state, or the lift keeps the distinguishing bit). It is a containment of the class the SOUNDNESS note names, pinned by that test; mununu#1056 stays open until a design reproduces the abstention on main.

## Not covered here

The lifting itself is unchanged (a cube may still keep dead states; the concretisation copes). rIC3's own traces, and the `ric3-bmc` member's, are not replayed through this simulator's new operators any differently than before (they already replayed, or carried their own `Replay::Unavailable` reason).
