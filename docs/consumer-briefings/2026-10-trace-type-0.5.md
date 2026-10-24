# Consumer briefing — 0.5 `Trace`: one counterexample shape on every RTL verb (2026-10)

> Source of truth: [`Trace`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every client that reads a counterexample. Eighth cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.4); the CTXDSL lassos follow in a second part.

## TL;DR

- **One shape.** Every RTL verb that returns a path now returns a `trace`:

  ```json
  { "kind": "finite|lasso|trap|unreachable|attack",
    "steps": [ { "cycle": 0, "state": [...], "inputs": [...], "faults": [...] } ],
    "loop_start": 3, "unreachable_target": [...], "replay": { "testbench": {...} } }
  ```

  The cells are the same `CexCellView` as before (`register`, `value`, `hex`, `bits`). The old shapes are gone, with no alias:

  | where | before | after |
  |---|---|---|
  | `sv verify-auto` rows (CLI `--json` and API) | `counterexample: {prefix, cycle, inputs, unreachable_target}` and `testbench` | `trace` (`finite`, `lasso` with `loop_start`, `trap` for a trap-certificate row, `unreachable`), the testbench under `trace.replay.testbench` |
  | `btor2 verify --fault … --target` (CLI and API) | `attack: {depth, faults[]}` (CLI) / `attack: [FaultEventView]` (API) | `trace` with `kind: "attack"`, one step per cycle, injections on `steps[].faults` |
  | `btor2 verify --witness` (CLI only) | `witness: {depth, states, inputs}` or `{unavailable_within_k}` | `trace` (`finite`), or `trace_unavailable_within_k`; **the API gains `witness` / `witness_max_k`**, so the witness is no longer CLI-only |
  | `btor2 cegar` / `sv cegar` | `counterexample: {steps: [{cube_index, valuation}], ends_in_trap}` | `trace` (`finite` or `trap`), each step's predicates as 1-bit cells |
  | `btor2 game` (recurrence, unrealizable) | `stall_lasso: {prefix, cycle, inputs}` | `trace` (`lasso`); `strategy` is unchanged |

- **Unchanged:** verdicts, the CLI's human-readable counterexample printout, and `strategy` objects. The Cytoscape counterstrategy graph stays a view.
- **Schemas:** `sv-verify-auto-response`, `btor2-verify-request` and `btor2-verify-response` regenerated; the new definitions are `Trace`, `TraceStep`, `TraceKind`, `TraceFault` and `TraceReplay`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter does not read counterexamples; the cheatsheet reads `counterexample` on some lanes | read `trace.steps[]` |
| **monono** | lanes that read `counterexample.prefix` / `inputs` or `btor2 verify --witness` output | read `trace.steps[].state` / `.inputs` |
| **mununu-ui** | the cegar viewer (`counterexample`), the game panel (`stall_lasso`), the btor2 verify panel (`attack`) | read `trace`; one renderer can serve all three |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the shape lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts read verdicts, not traces |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not emit traces |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | cheatsheet reads |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | trace reads |

## Verification steps for every consumer

```bash
mununu sv verify-auto top.sv --top top --json | jq '.properties[] | select(.trace) | {name, kind: .trace.kind, steps: (.trace.steps | length)}'
mununu btor2 verify d.btor2 --witness | jq '.trace.kind, (.trace.steps | length)'
```

## Provenance

- Branch `feat/trace-type-0.5`. Tests: `a_trace_names_its_kind_from_its_shape` (finite, lasso with `loop_start`, trap, unreachable, attack, the JSON tags), the schema conformance test now walks `trace.steps[].inputs`, the handler suites under all features, and the CLI tests.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The CTXDSL lassos (`lasso_traces`, `counterexample_trace`, `deadlock_traces`, the sanity deadlock paths) adopt `Trace` in the second part, with `state_name` and `labels`.
