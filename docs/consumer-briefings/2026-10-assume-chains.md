# Consumer briefing — bounded-sequence assumptions are applied as preconditions (2026-10)

> Source of truth: [`AssumeShape::Chain`](../../crates/mununu-core/src/adapter/btor2/assume_monitor.rs) — surface: (CLI+API)

**Audience:** every client of `sv verify-auto` (CLI and `/api/v1/sv/verify-auto`) whose SVA carries `assume` statements: rosf, monono, mununu-ui. A verdict-scope change under the default `--assumes precondition`; no field changes.

## TL;DR

- **New shape applied.** An assumption `A1 ##1 … ##1 An |=> B`, the form `A[*n] |=> B` translates to, is now applied as a precondition, with one latch per sequence step. Before, it was listed under `assumptions.unapplied` ("only `assume property (B)` and `A |=> B` over booleans…") and the guarantees were checked without it.
- **What changes.** A guarantee that relies on such an assumption, for example a watchdog that never fires under the datasheet promise `(!heartbeat)[*7] |=> heartbeat`, can move from VIOLATED (or ⊥) to HOLDS, with the assumption now listed under `assumptions.applied` and the verdict scoped to the traces it admits. The `unapplied` reason text now names the bounded sequence too.
- **Soundness.** The monitor is exact for the shape: the obligation falls due in the cycle the sequence ends, and a break in the sequence cancels the attempt. Its `$past` warm-up uses the deepest history read anywhere in the chain, which arms later and weakens the assumption, never strengthens it.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a lane with a bounded-sequence `assume` may report HOLDS where the guarantee depends on it | re-run; the row's `assumptions.applied` names it |
| **monono** | the same, on any SVA file with such an `assume` | re-run `make formal-docker` |
| **mununu-ui** | none | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | verdict scope |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling reads verdicts and timings |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/*` | at the next pin | verdict scope |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | at the next pin | verdict scope |

## Verification steps for every consumer

```bash
mununu sv verify-auto sensor_frontend.sv --source sensor_rx.sv --source sensor_rx_contract.sv \
  --source sensor_monitor.sv --source sensor_monitor_sva.sv --top sensor_frontend --pin rst_n=1 --json \
  | jq '.assumptions.applied'
#   includes m_heartbeat_every_8, the `(!rx_heartbeat)[*7] |=> rx_heartbeat` datasheet promise
```

## Provenance

- Found preparing the EmTech tutorial recordings (segment 3: a vendor black box's datasheet restated as assumptions). Tests at the seam: `adapter::btor2::assume_monitor::tests::a_bounded_sequence_antecedent_is_a_chain` (including the translator's own spelling) and `…::a_chain_assumption_obliges_the_consequent_when_the_sequence_ends`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Unbounded antecedents (`##[m:$]`, `[*n:$]`), goto / non-consecutive repetition, and liveness assumptions, which stay unapplied and reported.
