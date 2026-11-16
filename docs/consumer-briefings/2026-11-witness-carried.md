# Consumer briefing — `btor2 verify --witness` ships the deciding member's counterexample (N5.4)

> **Audience:** maintainers of the repositories that consume `mununu btor2 verify --witness` or `POST /api/v1/btor2/verify` with `witness: true` (rosf, monono, mununu-ui). Not a release note.
>
> Source of truth: [`witness_for_violation`](../../crates/mununu-core/src/adapter/btor2/witness.rs) · [`ReachMembers::witness`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API

## TL;DR

With `--witness` / `witness: true`, the `trace` of a `violated` verdict is now the counterexample the **deciding member found**, replayed on the concrete simulator, instead of a trace re-derived afterwards by native BMC from depth 0. Two JSON fields are new (`trace_by`, `trace_replay_failed`); nothing was removed, no verdict value changes, and a call without `witness` behaves exactly as before.

What changes for a consumer:

1. **`trace` is not necessarily the shallowest path any more.** It is the deciding member's own (`sat-bmc`, `ic3`, `btormc`, `ric3`, `ric3-bmc`), cut at the frame that makes `bad` true. Only a trace with `trace_by: "native-bmc"` is the shallowest. Read the depth from the trace (`steps.len() - 1`), never assume it.
2. **Far more `violated` rows ship a trace.** Before, the re-derivation reached the depth on the shallow rows only (HWMCC'20 `circular_pointer_top_w16_d64_e0`: the verdict at 168 s, native BMC at depth 16 when the 300 s budget ran out, `trace: null`). Now the member that found the row carries its trace at no extra search.
3. **`trace_by` may differ between two runs of one design.** Which members finish before the portfolio's cancel is a matter of timing; the pick among the finished members is deterministic (the fixed engine order), the set of finished members is not. Each is a replayed counterexample of the design.
4. **`trace_replay_failed` is evidence against a member's verdict.** A member that found `bad` reachable but whose own witness did not replay on the design is listed with the reason; `sole_decider: true` marks the member whose verdict is the whole `violated`. The verdict is unchanged — treat such a row as unconfirmed and file it upstream (one ask, one issue).

## Per consumer

### rosf · monono (CLI and HTTP)

- `trace` semantics: the deciding member's path; the field's shape (`kind: "finite"`, `steps[]`) is unchanged.
- New `trace_by: string | null` (CLI prints `null` without a trace; the API omits the field).
- New `trace_replay_failed: [{member, reason, sole_decider}]` (CLI prints `[]`; the API omits an empty list).
- `trace_unavailable_reason` now names every member the fallback ran and its miss, then native BMC's; `trace_unavailable_within_k` is still set only when native BMC searched the whole `--witness-max-k` bound.
- A call without `witness` runs the members on their default path: no replay work, no new fields with content.

### mununu-ui

No CTXDSL surface; no change required. If the UI shows a BTOR2 counterexample, show `trace_by` beside it.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the carried trace and the two fields |
| `mununu-sva` (dev / e2e) | yes | same; the e2e `e2e_circular_pointer_w16_d64_carries_the_ric3_bmc_trace` runs here |
| consumers' own images | when they pin the `btor2 verify` JSON shape | `docs/api-schemas/btor2-verify-response.schema.json` gained `trace_by` and `trace_replay_failed` |

## Provenance

N5.4 of the owned-IC3 lane (Track N), after mununu#1045 / #1046 (the witness route's time budget). Measured on the box in `mununu-sva`: rIC3 `-e bmc --bmc-dyn-step --witness` prints its witness at a cost — `arbitrated_top_n2_w8_d16_e0` 4.3 s → 9.4 s (a 32,911-line witness) — which is why the members print one only when `witness` is asked for.

## Not covered here

`sv verify-auto`'s safety witness (`reach_rescue::safety_witness`) still re-derives through rIC3 `--witness` then native BMC; routing it through the carried trace is the lane's next step. Pono, native and exact carry no trace yet (the fallback covers rows they decide).
