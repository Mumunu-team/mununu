# Consumer briefing: a seventh reach-portfolio member, `ric3-bmc` (2026-11)

> Source of truth: [`ric3::decide_via_ric3_bmc_dyn_step`](../../crates/mununu-core/src/adapter/ric3/mod.rs) — surface: CLI+API

**Audience:** anyone who reads `reachable_by`, `refused`, `abstained` or the verify-auto `portfolio` block, from `btor2 verify`, `sv verify-auto` or the liveness and rescue verbs that run the full reach portfolio.

## TL;DR

- **New member.** The full reach portfolio now runs `ric3-bmc`, which is rIC3's bit-level BMC with the dynamic step (`rIC3 -e bmc --bmc-dyn-step`).
  - The dynamic step asks for `bad` anywhere in a growing window, instead of visiting every depth in turn.
  - On HWMCC'20 `mul9` (mununu#997) it is the only member that decides the row: SAT at depth 238, in 62 s, where every other member gives no answer at 300 s.
- **Counterexamples only.** The member only ever appears in `reachable_by`. A bounded search proves nothing, so an `UNSAT` from it is read as `unknown`. It never appears in `unreachable_by`.
- **Where it can appear.** The new name can show up in `reachable_by`, `refused` (for example, rIC3 absent on the host) and the verify-auto `portfolio` lists.
- **Where it does not run.** It is not in `--owned-only`. It is not in the proof-directed member set of the k-liveness rungs.
- **Verdicts.** A row may now decide `violated` where it was `unknown`. No `holds` changes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `"ric3-bmc"` is a new member name in the lists. | If a gate enumerates member names, add it. It is an external (subprocess) member. |
| **monono** | Same. | Same. |
| **mununu-ui** | Member names render as text. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. rIC3 1.5.2 is already in the `mununu-sva` toolchain. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#997, the falsifier table of 2026-11-12, measured with rIC3 1.5.2 in `mununu-sva`.
- **Tests:**
  - `the_bmc_member_never_reports_a_safety_proof` (the verdict map);
  - `e2e_the_dynamic_step_bmc_member_finds_counterexamples_and_never_proves` (in the image).
- **Sibling sites:** the member rosters (`Members`, `ReachMembers::{ALL, PROOF_DIRECTED}`), `collect`'s order and lists, the owned-only exclusion test, and verify-auto's subprocess-member check.

## Not covered here

- The owned lever for this row class, a bit-level SAT BMC with a dynamic step, is roadmap N6's CNF emitter plus CaDiCaL. It is not part of this change.
- `--witness` re-derives the trace with native BMC up to `--witness-max-k`, default 200. A row decided only by `ric3-bmc` at depth 238 reports `trace_unavailable_within_k` unless the bound is raised.
