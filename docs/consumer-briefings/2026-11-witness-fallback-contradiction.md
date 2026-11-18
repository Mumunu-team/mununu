# Consumer briefing — a member that proves `bad` unreachable during the witness search is a contradiction, not a note (mununu#1076)

> **Audience:** maintainers of the repositories that read a `violated` verdict from `mununu btor2 verify --witness`, `POST /api/v1/btor2/verify`, `mununu sv verify-auto` (rows, `bottom_reason`, notes) or the fault adversary (`--fault`) — rosf, monono, mununu-ui. Not a release note.
>
> Source of truth: [`ShippedWitness::contradicted_by`](../../crates/mununu-core/src/adapter/btor2/witness.rs#L499) · [`apply_fallback_contradiction`](../../crates/mununu-core/src/adapter/btor2/witness.rs#L508) · [`WitnessMiss::contradiction`](../../crates/mununu-core/src/adapter/reach_rescue.rs#L652) · [`contradicted_row`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L2013) — surface: CLI+API

## TL;DR

When a verdict is `violated` and the witness search runs the trace-carrying members again to derive a path (N5.4's fallback: `sat-bmc`, `ic3`, btormc, rIC3, then native BMC), one of those members can PROVE `bad` unreachable. That is a soundness alarm: one of the two engines is wrong. Until now the alarm reached the report only on one path (a `witness-unavailable` note on a row still shipped `violated`), and on the other path (a trace found by another member beside the proof) it was a stderr line and the JSON carried nothing (the two runs cited in mununu#1076).

Now the report applies the same rule the reach portfolio applies to its own members disagreeing:

1. **`btor2 verify --witness` (CLI JSON) and `POST /api/v1/btor2/verify`:** the verdict becomes the contradiction — `outcome: "unknown"`, `contradiction: true`, the proving member added to `unreachable_by` beside the members in `reachable_by` — and **no trace ships** (a path found beside a proof of unreachability is not evidence); `trace_unavailable_reason` starts with `contradiction: <members> proved `bad` unreachable …`. No field is added or removed; `docs/api-schemas/` is unchanged.
2. **`sv verify-auto` rows:** the row leaves `violated` for `unknown` with `bottom_reason: engine-contradiction` whose `detail` names both sides (`Violated@[<decided_by>] vs Holds@[<members>]`), a `bottom-reason` note on the property, and no `counterexample`. The `witness-unavailable` note is not written for this case (it described a verdict that stands; this one does not). The coverage summary and the exit code follow the row. One distinction the row path keeps: the witness search runs on the bounded safety-witness MONITOR, which decides the formula only when every `||` in its block has at most one box-bearing side (`chain_is_exact`). For a `[] a || [] b` block the one-path reading can be unreachable while the branching reading fails, so a proof of unreachability there without a path beside it is the existing `witness-unavailable` miss (stage `shape`, "a violation with no single-path witness"), not an alarm; with a path found beside the proof, or on an exact block, it is the contradiction (the path search's miss is stage `unreachable`, naming the prover; the row's reader never sees that stage because the row leaves `violated` instead).
3. **The fault adversary (`--fault`, `--target`):** `verdict: "unknown"`, both sides in `decided_by`, the alarm in `trace_unavailable_reason`, no attack.
4. **Trap certificates** (recoverability): a certificate whose path search is contradicted is reported as `contradiction`, not `certified`.

Every path that can raise the alarm now reaches the report: with or without a trace beside the proof, on the carried-trace route and on the re-derivation route.

## What does not change

A member that merely finds nothing, refuses the design, stops on its budget or carries no trace is still a miss (`witness-unavailable`, `trace_unavailable_reason`), and the `violated` verdict stands on those, as before (mununu#803, mununu#1045). A contradiction INSIDE the verdict's own run was already the contradiction verdict; this briefing is about the second run, the witness search.

## Per consumer

### rosf · monono (CLI and HTTP)

- A `violated` with `contradiction: true` and a non-empty `unreachable_by` can now come out of a run that asked for a witness; a consumer that counted `violated` rows for a gate reads `outcome` (it is `unknown`) and `contradiction`. On `sv verify-auto`, read `bottom_reason.kind == "engine-contradiction"` the way mununu#553 / #728 describe: a soundness alarm where retrying is wrong — it is a bug report, not a budget question.
- No schema change; no route or flag changes.

### mununu-ui

No CTXDSL surface; no change required. A view that shows a `violated` row's path may now see the row as `unknown` with the contradiction detail instead.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the verdict rule |
| `mununu-sva` (dev / e2e) | yes | same |
| consumers' own images | no | no schema change |

## Provenance

The two runs in mununu#1076 (the cut-point run of mununu#1062 on main `6831311e`: `violated` with a `witness-unavailable` note naming `ic3`'s proof; run A of mununu#1061: `ERROR witness fallback: … member="ric3"` on stderr and nothing in the JSON). Seam reproducers: `the_fallback_names_the_members_that_proved_bad_unreachable` (witness: the fallback's reading of a second portfolio run, with and without a trace beside the proof, and the report rule) and `a_contradicted_path_search_takes_the_row_to_unknown_with_both_sides_named` (verify-auto: the row downgrade). The seam they drive is introduced by this change, so they cannot run on the parent; the issue's two runs are the evidence that the parent shipped `violated`.

## Not covered here

The two further members of the class the issue lists — a violation read at an initial cube the cube lift kept only on doubt (an `Unknown` / expired / un-encodable cycle-0 query), and an unnamed next-less state in a property atom's cone that the exact engine reads instead of refusing — are tracked as mununu#1085 and mununu#1086; they live in the verdict path of those engines, not in the witness search.
