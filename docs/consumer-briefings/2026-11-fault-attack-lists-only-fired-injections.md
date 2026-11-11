# Consumer briefing: a fault attack lists only the injections that fired (2026-11)

> Source of truth: [`gated_fires`](../../crates/mununu-core/src/adapter/btor2/fault.rs#L949) — surface: CLI+API — `btor2 verify --fault … --target ATOM` (the `attack` list), the fault form of `POST /api/v1/btor2/verify`, and the repair localiser's override frames under `sv verify-auto --localize`.

**Audience:** monono (sec_cm fault runs), rosf, mununu-ui, and anyone who reads a fault attack or a repair candidate's cycles.

## TL;DR

- **Verdicts do not change.** `holds` / `violated` / `unknown` and `decided_by` are exactly as before.
- **The `attack` list of a `violated` fault run now names only the injections the fault gate let fire.** Before, it listed every frame of the witness whose fault-enable input was 1, including frames inside the warm-up, frames past the budget, and the last frame (whose inputs drive no transition). A witness from rIC3 or btormc sets the enable on blocked frames, so on a host with those tools an attack could list more faults than the budget allowed. Measured in `mununu-sva` (mununu#981): 3 "faults" under a budget of one, one of them at cycle 1 under a 2-cycle warm-up. The attack is now at most the budget and never inside the warm-up.
- **The repair localiser** (`--localize`): a candidate's `cycles` / `values` now list only the frames on which the override fired under the query's budget, by the same rule.
- **Unchanged:** the per-step `faults` on a verify-auto counterexample trace already applied the gate (`trace.rs`). It still shows an injection that fires on the last step, which the attack list drops because it cannot contribute to the violation.
- **No shape, flag or route changes.**

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | A fault run's `attack` can be shorter than before, never longer, and always within the declared budget and warm-up. | If a recorded attack was compared length-for-length, re-record it. |
| **rosf** | Same, if you run `btor2 verify --fault --target`. | None required. |
| **mununu-ui** | No fault UI. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle | Yes, to get the corrected attack lists. | The behaviour comes from the binary. |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, if you run fault gates in the image (the subprocess members that produce such witnesses live there). | The binary on the volume changes; the toolchain does not. |
| `mununu/docker/Dockerfile.dev` | Only if your dev workflow runs fault verbs. | — |
| `mununu/docker/Dockerfile.profile`, `Dockerfile.extract*` | No. | They don't run fault verbs. |
| rosf `docker/Dockerfile*` | Only if rosf runs fault verbs. | Picks up the change. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Yes, if monono pins a mununu version and runs sec_cm fault rows. | Picks up the change. |

## Provenance

- **mununu#981.** `adapter::fault_adversary::tests::one_fault_escapes_the_trap_and_the_witness_names_it` and `redundancy_defeats_a_single_fault_and_falls_to_two` failed in `mununu-sva` and passed on the GitHub runner, where the subprocess members are absent and native BMC's witness happened to leave the blocked enables at 0.
- **Tests:** `adapter::btor2::fault::tests::an_enable_the_gate_blocks_is_not_reported_as_a_fault` (a hand-built trace with enables on blocked frames, plus a negative control that counts the old rule's four events), and the two `fault_adversary` tests in the image.

## Not covered here

- The other eight tests of mununu#981 (the yosys / multi-module lift class, `reach_rescue`, `escalate_bottom`, the refine interpolant) and the runner-vs-image gate question go in separate PRs.
