# Consumer briefing — 2026-10 a recoverability guarantee says, on its own property, what its witness says about it; the report self-check grows its HOLDS side and runs on the merged report

> **Audience:** monono first — this is your [Ask 21](https://github.com/Mumunu-team/mununu/issues/599) from wave 1's `sprite_fetch`. Also ROSF and any consumer that gates on `sv verify-auto` verdicts for `@mununu_guarantee` recoverability properties.
>
> **Related:** closes [mununu#599](https://github.com/Mumunu-team/mununu/issues/599). Extends the self-check from [#583](https://github.com/Mumunu-team/mununu/pull/583) (`report-self-contradiction`, briefing [`2026-09-annotation-names-and-report-self-checks.md`](2026-09-annotation-names-and-report-self-checks.md)). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **additive wire-shape change + verdict-semantics change.** (1) A new optional field `vacuity` on a property verdict: a recoverability guarantee `AG EF(P)` that HOLDS now carries what the other verdicts of the same report say about it — `invariant-target` (proven vacuous: the design never leaves `P`) or `witness-refuted` (a same-register reachability is VIOLATED here, so the guarantee is not shown non-vacuous). The verdict does not change. (2) The report self-check admits the HOLDS-side contradictions (`EF(P)` HOLDS beside `AG(¬P)` HOLDS, `AG EF(P)` HOLDS beside `EF(P)` VIOLATED, …) and now also runs on the **merged** portfolio report; a pair it hits is withheld as `⊥` with `report-self-contradiction`, both halves, no winner.

## 1. `vacuity` — the ask, answered at the property

Your run under `--cutpoint fits --cutpoint left_q`:

```
ann_guarantee_0   AG EF (st_q == S_IDLE)    HOLDS
ann_guarantee_1   EF  (st_q == S_WAIT)      VIOLATED   <-- the witness
```

That pair is **consistent** — nothing in it is a contradiction (`st_q` may leave `S_IDLE` and come back without ever visiting `S_WAIT`), so no verdict is withheld. What was missing was the statement at `ann_guarantee_0` that a reachability over its own register is refuted in the same run. It is there now:

```json
{
  "name": "ann_guarantee_0",
  "outcome": "holds",
  "vacuity": {
    "kind": "witness-refuted",
    "witness": "ann_guarantee_1",
    "detail": "NOT SHOWN NON-VACUOUS — `ann_guarantee_1` (`mu Z.((st_q == 3) || <> Z)`), a reachability over this guarantee's register, is VIOLATED in the same report. …"
  }
}
```

and on the CLI, on the guarantee's own lines:

```
  [assert] ann_guarantee_0: holds
        formula: nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)
        vacuity [witness-refuted] witness=ann_guarantee_1: NOT SHOWN NON-VACUOUS — …
```

| `vacuity.kind` | established by | what to do |
|---|---|---|
| `invariant-target` | `EF(¬P)` VIOLATED or `AG(P)` HOLDS in the same report | The guarantee is **vacuous**: `P` is invariant, no recovery is ever exercised. The HOLDS stands and proves nothing about recovery. Fix the witness or the property. |
| `witness-refuted` | `EF(Q)` VIOLATED with `Q` over `P`'s register at another value | The guarantee is **not shown non-vacuous** by this run. It is not false. If that property is the witness you listed for this guarantee, the pair is a failed gate, not a pass — exactly what `rtl/vpu/sprite_fetch/verify.sh` already encodes by hand. |

Absent on every other property, and on a guarantee no other property speaks to. The notes stream carries the same finding as a `vacuous-guarantee` note with `property` set (the structured join), level `scope-caveat`.

**What the tool does not do.** It does not decide which half of your pair is right — `S_WAIT` being "plainly reachable in the concrete design" is knowledge the report does not have. Whether the `VIOLATED` is an unsound cut or the slice genuinely removes the path is a question for the cut-point contract (a violation under a cut may be spurious; the `control-slice` note says so), and the new field makes the pair visible at the place a gate reads.

## 2. The self-check's HOLDS side, and the merged report

`report-self-contradiction` (#583) admitted one pair: `AG(P)` VIOLATED beside `EF(¬P)` VIOLATED. The table is now every pair of **definite** verdicts that cannot share one model with a non-empty initial set, over the same or exactly negated single-comparison atom:

| a | b | why |
|---|---|---|
| `EF(P)` HOLDS | `AG(¬P)` HOLDS | `∃s.P` against `∀s.¬P` |
| `AG EF(P)` HOLDS | `EF(P)` VIOLATED | the initial state is reachable, so `EF P` holds there |
| `AG EF(P)` HOLDS | `AG(¬P)` HOLDS | `EF P` at the initial state against `P` nowhere |
| `AG EF(P)` VIOLATED | `AG(P)` HOLDS | `P` invariant makes `EF P` true everywhere |
| `AG EF(P)` VIOLATED | `EF(¬P)` VIOLATED | `¬P` unreachable is `AG P` |

Same disposition as before: **both withheld, no winner**, `determinism: reproducible`, escalate rather than re-run. Nothing weaker is admitted — the vacuity relation above is deliberately *not* a contradiction, and #579's own bound/witness rule stays unimplemented for the reason recorded in #583's briefing.

**The merge was unchecked.** Each engine's report was checked against itself; the merged portfolio report — where a property decided by the exact engine sits next to one decided by the cube, which is how #577's pair came to exist — was not. It is now, after the merge's other post-passes, and the `vacuity` fields and notes are re-derived there too.

## What to update, per consumer

### monono

- `rtl/*/verify.sh` gates that pair a recoverability guarantee with its witness by hand can read `vacuity` instead: `jq '.properties[] | select(.vacuity != null)'`. A `witness-refuted` on a guarantee whose witness you listed is your failed gate, now stated by the tool.
- Your JSON parser must tolerate the new optional key (additive; absent when there is nothing to say). A strict schema consumer regenerates from `docs/api-schemas/sv-verify-auto-response.schema.json`.
- The coverage-summary count you also raised in Ask 21 ("5 definite … 3 skipped" against "8/8 decided") was fixed in [#539](https://github.com/Mumunu-team/mununu/pull/539) (`refresh_coverage_summary`, 2026-09-10): the summary is recomputed after the merge.

### ROSF and others

- No change unless you parse `sv verify-auto` JSON strictly or gate on recoverability guarantees. A report that previously shipped a HOLDS–HOLDS contradiction now ships two `⊥` instead; that is a soundness alarm you want to see.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | new field; merged-report self-check | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The relation and the pass, at the layer they live in:
cargo test -p mununu-core --lib --features api -- report_consistency x599_

# 2. Your sprite_fetch run, re-read:
mununu --quiet sv verify-auto sprite_fetch.sv --cutpoint fits --cutpoint left_q --json \
  | jq '[.properties[] | {property: .name, outcome, vacuity}]'
```

## Not covered here

- A **declared** witness link (`@mununu_guarantee … witness=<name>`) would let the tool judge the general case rather than the same-register heuristic; not in this change — say so if the heuristic misses your shape.
- `vacuity` is computed for the recoverability shape `AG EF(P)` over a single comparison atom. Response-liveness (`AG(p → EF q)`) and compound targets carry nothing yet.
