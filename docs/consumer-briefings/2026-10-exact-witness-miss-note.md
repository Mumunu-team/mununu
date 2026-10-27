# Consumer briefing — a refuted liveness row the exact engine cannot attach a lasso to says why (2026-10)

> Source of truth: [`LadderOutcome::witness_miss`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + [`witness_miss_note`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone who reads `notes[]` beside a VIOLATED liveness or recoverability row. The mununu#808 follow-up (the lasso-miss half of mununu#803's silence rule).

## TL;DR

- **No verdict changes.** When the exact engine refutes an `AF p` / `AG AF p` / `AG EF p` row and the lasso extraction then fails (an exhausted BDD arena, a panic caught at that site), the verdict already stood and the trace was already absent; until now the reason went to the log only. The row now carries the existing `witness-unavailable` note (`kind: "witness-unavailable"`, `level: info`, `property: <row>`, `items: ["exact-witness"]`, the reason in `detail`) — the same note the safety path ships when its path finders decline (mununu#803, stage names `monitor` / `parse` / `search` / `bound` / `shape`).
- **Additive.** No JSON shape change, no schema regeneration: one more note kind value on an existing field. A consumer that already treats `witness-unavailable` as "VIOLATED without a path, verdict stands" needs nothing; one that keyed on the stage names gains `exact-witness`.
- A row whose formula has no one-path witness shape at all (neither `AF` nor `AG EF`) still ships no note: nothing failed there.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `notes[]` may carry one more `witness-unavailable` entry on a refuted liveness row; `judge()` reads verdicts, not notes | none |
| **monono** | same; the formal lane reads `verdict` | none |
| **mununu-ui** | one more note on a row | none (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the note is emitted by the binary |
| `mununu/docker/Dockerfile.dev` | no | unchanged dev workflow |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | nothing in the image's tools changed; the unit tests below run on the host |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | one more note |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | result |
|---|---|
| `the_ladder_reports_why_a_violated_verdict_ships_no_lasso` (exact engine, inline BTOR2) | with the extraction working: `Violated` + lasso, `witness_miss: None`; with the failure injected at the extraction closure: `Violated`, no lasso, the reason carried |
| `rescue_skipped_via_exact_says_why_a_violated_row_ships_no_lasso` (verify_auto, inline BTOR2) | the row is VIOLATED both ways; with the failure injected the `witness-unavailable` note names the row, `exact-witness`, and the reason; without it the lasso ships and no such note exists |

## Provenance

- Fix commit: pending merge — branch `fix/exact-witness-miss-note`.
- Policy: [`docs/policies/failure-paths.md`](../policies/failure-paths.md) ("silence is a defect"), [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- `btor2 verify-recoverability`'s attribution object (`RecoverabilityAttribution`) still carries the trace only; no consumer renders its reasons today.
- The safety path's own path finders (unchanged since mununu#803).
