# Consumer briefing — `verify-recoverability` returns the trap as a `trace` (2026-10)

> Source of truth: [`RecoverabilityAttribution::trace`](../../crates/mununu-core/src/adapter/recoverability.rs) — surface: (CLI+API)

**Audience:** every client of `btor2 verify-recoverability` / `sv verify-recoverability` (CLI JSON and `/api/v1/{btor2,sv}/verify-recoverability`). Additive: no verdict or existing field changes.

## TL;DR

- **New: `trace`** on a `violated` recoverability verdict decided by the exact engine — the 0.5 `Trace` shape (`kind: "lasso"`): reset → a state from which the target is unreachable → the cycle the run stays in (`loop_start`), each step with its `state` and `inputs`.
- **Why.** The exact engine always produced this witness (it is what `sv verify-auto` shows for an `AG EF` annotation); the recoverability verbs computed and discarded it, so a violated verdict came with no path at all.
- **Names.** `sv verify-recoverability` gives the design's register names, the verifier's cells under `steps[].monitor` and no clock among the inputs ([readable traces](2026-10-readable-traces.md)); `btor2 verify-recoverability` keeps the BTOR2's names and still moves the verifier's cells to `monitor`.
- **Absent** when the verdict holds, is unknown, or was reached by the cube ladder (no concrete path).
- **Schemas:** `btor2-verify-recoverability-response` and `sv-verify-recoverability-report` gain the optional `trace`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | does not call the recoverability verbs | none |
| **monono** | recoverability lanes that print a failure can show the path | optional |
| **mununu-ui** | a recoverability result can reuse the `Trace` renderer | optional |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | new response field |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling reads verdicts |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/*` | at the next pin | additive |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | at the next pin | additive |

## Verification steps for every consumer

```bash
mununu sv verify-recoverability rod_drive_ctrl.sv --top rod_drive_ctrl --target "state_q == 0" --pin rst_n=1 \
  | jq '{outcome, kind: .trace.kind, loop_start: .trace.loop_start, cycle: [.trace.steps[.trace.loop_start:][] | .state[] | select(.register == "state_q") | .value]}'
#   outcome "violated", a lasso whose cycle sits in FAULT (state_q = 5)
```

## Provenance

- Found preparing the EmTech tutorial recordings. Test at the handler seam both surfaces share: `api::handlers::…::a_pinned_reset_decides_recoverability_out_of_reset` (the pinned refutation carries a lasso whose cycle stays in the trap; a HOLDS carries none).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- A trace for a refutation the cube ladder reaches (abstract cells, no concrete path).
