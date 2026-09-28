# Consumer briefing — 2026-09 a note says which property it is about, and the pass that was supposed to clean stale ones never ran

> **Audience:** monono — this is your ask 26 / mununu#548, and the part of it we told you was already fixed was not. rosf, if you read `verification_notes`.
>
> **Related:** closes [mununu#548](https://github.com/Mumunu-team/mununu/issues/548) (O-2, the last open part).
>
> **TL;DR:** **additive field, plus a real bugfix.** `verification_notes[i].property` names the property a note is about (absent for model-level notes) — **group by it instead of parsing `summary`**. And `drop_stale_bottom_reasons`, which we shipped as cause (a)'s fix in `cc28d9c`, **has never fired**: it matched a string format the code does not emit. So the defect you reported — a report explaining why a property is ⊥ while also reporting it HOLDS — was still live until now.

## ⚠️ The correction you are owed

When we closed cause (a) of #548, `drop_stale_bottom_reasons` was described as an unrequested bonus: it removes a ⊥ explanation for a property another engine went on to decide. It does not, and never did.

```
the pattern it matched:   "p: "
the summary we emit:      "`p`: residual ⊥ classified as `safety-shape-not-reducible` — …"
```

Every summary the note builder produces is **backtick-quoted**, and the pass built its pattern without backticks. `starts_with` is false for every real note, so the pass has been a no-op since the day it was written.

**It looked healthy because its own test invented the summary** as `"p: ⊥ — safety-shape-not-reducible"` — without the backticks the production builder emits. A green test sat on top of a dead check, which is the failure mode our own guidance warns about: a contract test is only as good as the fidelity of its mock, and a fabricated one tests the assumption rather than the system.

So: **the thing you reported was not fixed by cause (a).** A merged portfolio report could still carry a stale ⊥ explanation beside a definite verdict. It is fixed now, and the fix is the field below rather than a better string match.

## The field

```json
{
  "kind": "bottom-reason",
  "level": "scope-caveat",
  "summary": "`sva_3`: ⊥ because engine `exact-symbolic` did not complete …",
  "detail": "…",
  "items": [],
  "property": "sva_3"
}
```

- Present when the note is **about one property** — `bottom-reason`, `engine-isolation`, `array-atom-unsupported`, `safety-rescue-declined`, `skip-diameter-bound`, `skip-bitblast-oom`, `plan-cost`.
- **Absent** for a model-level note — `reset-gating`, `control-slice`, `parameter-override`, `config-concretization`, `coverage-summary`, `abstraction-posture`. Those describe the lift, not a property, and giving them one would be a lie you would then group by.

**Group on this field, not on the prose.** The name appeared only inside `summary` before, so any consumer joining on it inherited the same fragility the internal pass did: reword the note's opening and the join silently stops matching, and a join that finds nothing is indistinguishable from a note that was never there.

`engine-isolation` also keeps its `items: ["property:<name>"]` entry for one release, so if you are already reading that, nothing breaks on the same commit that gives you the better route.

## What to do

```bash
# every note explaining a specific property, grouped properly
mununu --quiet sv verify-auto design.sv --json \
  | jq 'reduce (.verification_notes[] | select(.property != null)) as $n
        ({}; .[$n.property] += [$n.kind])'

# model-level notes — the scope caveats that apply to the whole run
mununu --quiet sv verify-auto design.sv --json \
  | jq '[.verification_notes[] | select(.property == null) | {kind, level, summary}]'
```

The second query is the one worth adding to a gate: a `soundness-caveat` with no property applies to **every** verdict in the run, which is easy to miss when notes are read per-property.

## Schema

`docs/api-schemas/sv-verify-auto-response.schema.json` is regenerated; the drift detector covers the new field. It is optional (`skip_serializing_if`), so an existing consumer that ignores unknown keys is unaffected.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **A verdict change.** No property's outcome moves. What changes is which *notes* survive a portfolio merge — previously none were removed, so you may see **fewer** notes on a run where an engine rescued a ⊥. That is the fix working.
- **The `sv` property verbs' schema.** Still ad-hoc; [mununu#541](https://github.com/Mumunu-team/mununu/issues/541) stays open for pinning those three summaries.
- **Notes carrying more than one property.** A note is about one property or none; nothing in-tree needs a set, and inventing one would be speculative surface.

---

**Provenance.** Issue: [mununu#548](https://github.com/Mumunu-team/mununu/issues/548) O-2, from monono's ask 26. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
