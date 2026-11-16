# Consumer briefing: `sv verify-auto` safety traces and fault-attack traces use the engines that found them (2026-11)

> Source of truth: [`search_witness`](../../crates/mununu-core/src/adapter/btor2/witness.rs) — surface: CLI+API — `mununu sv verify-auto` (safety counterexamples), `mununu btor2 verify --fault … --target …` and `POST /api/v1/btor2/verify` with `faults` + `target`

**Audience:** anyone who reads `counterexample` on `sv verify-auto` safety rows, or the attack `trace` of a `btor2 verify --fault … --target` call.

## TL;DR

- **Two more callers now use the N5.4 trace search (#1049).** That is the `sv verify-auto` safety counterexample, also used by the trap certificate, and the fault adversary's attack trace. Both used to re-derive the path with rIC3 under the rescue slice and then native BMC. Now the trace-carrying members race on the monitored or injected design: owned `sat-bmc` / `ic3`, plus btormc, rIC3 and rIC3's BMC where the image has them. Every witness is replayed on that design before it ships, and native BMC runs last. Deep counterexamples that native BMC could not reach in the slice now ship.
- **The search stays inside the budget it runs under.** In `verify-auto` that is the per-property budget or rescue slice. With no deadline of its own, the search used to install an unbounded budget over the caller's, so a deep search could run past the property budget.
- **Fault adversary, `--target` (CLI) / `target` (API):**
  - The attack now names its member: `trace_by` on CLI and API.
  - A search that finds no attack path now says why, in `trace_unavailable_reason`. Before, it was only a log line.
  - A `violated` verdict without a found path no longer ships an empty `attack` trace (`trace: {kind: "attack", steps: []}`): `trace` is `null` / omitted, with the reason.
  - `--timeout-ms` / `timeout_ms` now bounds the attack's trace search.
  - The API now honours `witness_max_k` on this path; it used a fixed 200.
- **No verdict changes.** A trace can be the deciding member's own and so not the shallowest (as #1049 says for `btor2 verify --witness`).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `sv verify-auto` safety rows ship a counterexample more often; the trace may be longer than the shallowest | none, unless you assumed minimal depth |
| **monono** | same; `btor2 verify --fault … --target` gains `trace_by` / `trace_unavailable_reason`, and an empty attack trace is now `null` | read `trace == null` + the reason instead of an empty `steps` list |
| **mununu-ui** | no UI surface for these verbs | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the routed traces and the fault-trace fields. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image. | They consume the binary. |

## Provenance

- **Follows:** #1049 (N5.4), the "not covered" list of #1046 (the `--fault` arms) and #1049 (`reach_rescue::safety_witness`).
- **Tests:**
  - `an_outer_run_budget_bounds_the_witness_search` (`witness.rs`);
  - `one_fault_escapes_the_trap_and_the_witness_names_it`, `the_plain_fault_arm_carries_its_attack_and_names_the_member` (`fault_adversary.rs`);
  - the existing `safety_witness` tests in `reach_rescue.rs` and `verify_auto.rs`.

## Not covered here

- `sv verify-auto` rows do not report which member supplied the counterexample (the SV report has no `trace_by`); `reach_rescue::safety_witness_by` returns it for a later report field.
