# Consumer briefing — 2026-10 `--expect-violated` / `--expect`: a label names a property in "every other property must hold" too, and a repeated label names every property carrying it

> **Audience:** anyone gating `sv verify-auto` with `--expect-violated` / `--expect` on the CLI, or `expectations` on `POST /api/v1/sv/verify-auto` — rosf (`examples/block_contract`, the claim re-judge) and monono (the contrast-twin gates) first.
>
> **Provenance:** [mununu#716](https://github.com/Mumunu-team/mununu/issues/716), found by rosf, whose independent reading of the same document disagreed with mununu's. Code: `adapter/slang/expectations.rs` (`evaluate`, `resolve`). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

A claim key resolved by **label or name**, but the completeness rules — "every property not named as violated must hold" and "an unnamed VIOLATED is still a failure" — matched the key against the positional **name** only. A twin pinned by its label therefore always failed the claim that named it (exit 4). Both halves now use one resolution: label first, then name, and **all** matches, so a label repeated across instances names every instance.

No verification verdict changes. What changes is the **gate's** verdict (exit code and `expectations` block) for claims that name a violated property by label, or name a label shared by several properties.

## What changed

| Claim | Before | After |
|---|---|---|
| `--expect-violated a_clr_lands` (label of a VIOLATED property, the rest hold) | exit 4, "not named as violated, so it must still hold" | exit 0 |
| `--expect a_clr_lands=violated` | exit 4, "VIOLATED but not named" | exit 0 |
| `--expect-violated sva.counter_noclr_sva_sva_0` (positional name) | exit 0 | exit 0 (unchanged) |
| a label shared by N properties | named the first only; the other N−1 counted as unnamed | names all N; each must have the claimed verdict, failures reported under the instance path |
| a key that is one property's label and another's positional name | resolved to the labelled property, completed against the other | names the labelled property in both halves (label first, as documented) |
| pinning a `holds` by label | worked | unchanged |

## What to update, per consumer

- **rosf** — `examples/block_contract` can pin the violated twin by label (`a_clr_lands`) again instead of by instance path. The claim re-judge and mununu's `expectations` block now agree on label-pinned violated claims; a disagreement there is again a real contradiction.
- **monono** — no action required. Existing positional pins resolve as before. A label pin on a property instantiated in more than one place now checks every instance; if a gate relied on only the first instance being checked, it will now report the others.
- **Any parser of the `expectations` block** — the JSON shape is unchanged. A shared-label claim can now produce more than one failure entry for one key, one per instance.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | gate exit code and `expectations` for label-pinned violated claims | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; where `sv verify-auto` runs | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles mununu; its block-contract example pins by label | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo test -p mununu-core --lib adapter::slang::expectations
mununu sv verify-auto twin.sv --source twin_sva.sv --top twin --expect-violated <label>
```

## Not covered here

- `--expect-all-hold` and `--expect-count` name no property and are unaffected.
- `absent_reason` still looks up untranslatable assertions by positional name, because an untranslated assertion carries no label in the report.
