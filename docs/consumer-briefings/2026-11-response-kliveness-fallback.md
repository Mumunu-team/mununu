# Consumer briefing — the k-liveness proof fallback for response liveness (Track V, 2026-11)

> Source of truth: [`response_kliveness`](../../crates/mununu-core/src/adapter/liveness_rescue.rs#L603), [`emit_response_kliveness_monitor`](../../crates/mununu-core/src/adapter/btor2/kliveness_monitor.rs#L124), [`undecided_notes`](../../crates/mununu-core/src/adapter/liveness_rescue.rs#L573) — surface: CLI+API (`btor2 verify-liveness` with `--request/--grant`, `--fairness` or `--response`; `sv verify-liveness`; `POST /api/v1/btor2/verify-liveness` and `POST /api/v1/sv/verify-liveness` in their response forms).

> **Audience:** anyone calling the response-liveness verbs (`AG(request → AF grant)`, with or without `GF` fairness, or a conjunction of responses) on the CLI or the HTTP API.
>
> **TL;DR — more proofs, one additive JSON field, no verdict can flip.** When l2s leaves a response row undecided, the verbs now try a k-liveness bound before reporting `unknown`. It is HOLDS-only: verdicts can move only `unknown → holds`. A proof reads `decided_by: ["kliveness", "k=<bound>", "<member>", …]`. An undecided row now carries a `notes` array saying why (each member's refusal or abstention, and the k-liveness note). Decided rows are serialised exactly as before (no `notes` field).

## What changed

| surface | item | meaning |
|---|---|---|
| CLI + API | verdict | an l2s-undecided row may now be `holds`; nothing else moves |
| CLI + API | `decided_by` | `["kliveness", "k=<2^w−1>", "<member>", …]` on a fallback proof |
| CLI + API | **new** `notes: [string]` (omitted when empty) | only on an undecided row: `"<member> refused: <why>"`, `"<member>: <why>"` (a budget abstention), and `"kliveness: no bound on a never-granted request proved … (k=1 reachable, k=3 undecided)"`. Present on `Btor2VerifyLivenessResponse`, `SvVerifyLivenessReport`, and each entry of `SvVerifyLivenessAllReport.responses` |
| CLI | `btor2 verify-liveness` JSON summary | the same `notes` key, only on an undecided row |
| time | undecided rows only | at most the width ladder: six widths, each under the members' 60 s cap, stopping at the first width no member decides. Decided rows never run it |

Not changed: the justice form of `verify-liveness` (its own rungs, unchanged), `verify-auto` (its response rescue does not call the fallback yet), and `verify-recoverability`.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | If you call `verify-liveness` and read `outcome` / `decided_by`: more `holds`, a new label in `decided_by`. If you deserialise the response strictly (unknown fields rejected), the new optional `notes` field appears on undecided rows. | Accept `notes` (optional, array of strings). |
| **monono** | Same as rosf if you call the response verbs; otherwise nothing. | As rosf. |
| **mununu-ui** | The response verbs are RTL verbs, not CTXDSL: no UI. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, to get the fallback. | Engine behaviour + one response field. |
| rosf `docker/Dockerfile*` | Only if you pin the mununu binary inside and want the fallback. | Consumer of the CLI/API. |
| monono `Dockerfile.dev` / `Dockerfile.vexriscv` | No, unless you call the response verbs. | Not a caller today. |

## Provenance

Track V (`mununu-private` roadmap, 2026-11), after V1 (the justice rung, #984: 15 new HWMCC'17 LIVE proofs at 300 s with zero regressions, 23 at 3,600 s). The monitor is the Büchi automaton for `F(request ∧ G ¬grant)`: a nondeterministic commitment to one request that dies on its first grant, under V1's round counter with the fairness atoms. A first draft counted cycles on which a request is pending. That draft was dropped before code because it never bounds a design that grants every request but keeps issuing them. Tests at the seam are solver-free: exhaustive reachability over the emitted monitor gives the largest round count any path reaches (a responder bounded at 1, a staller unbounded, a fairness-gated design unbounded without `GF go` and bounded at 2 with it). The proof reach is measured on the wall-class matrix (`kl-af` column, the fallback on each case's `AG(¬target → AF target)`).

## Not covered here

The planner wiring (verify-auto's `escalate_bottom`) and the recoverability pre-check (`AG EF good` via the strengthening `AG(¬good → AF good)`) are Track V's V3 / V4. mununu#980 (rIC3 aborting on the `justice` line the l2s monitor keeps) is a separate fix; the k-liveness monitor already drops those lines.
