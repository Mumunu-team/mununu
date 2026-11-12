# Consumer briefing — a constraint disjoint from the property's cone is no longer dropped (2026-11)

> Source of truth: [`cone_closure`](../../crates/mununu-core/src/adapter/btor2/dep_graph.rs#L336), [`bit_cone`](../../crates/mununu-core/src/adapter/btor2/dep_graph.rs#L564) and [`cone_slice`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L1072) — surface: CLI+API (`mununu btor2 verify-liveness --justice`, `POST /api/v1/btor2/verify-liveness`).

> **Audience:** everyone who reads a justice / fair-cycle verdict from mununu — rosf and monono (CI gates in the `mununu-sva` image), mununu-ui, and agents on the HTTP API.
>
> **TL;DR — a soundness bugfix: the exact justice rung could report `violated` on a design whose `constraint` lines leave no infinite path, when every signal the constraint reads lies outside the justice literals' cone.** The cone slice dropped such a constraint. It is now always kept, as is every `fair` line. On the affected shape, the verdict changes from `violated` (wrong) to `holds`. No other verdict changes on the gate: 58 HWMCC'17 LIVE rows with a constraint or fair line give the same verdicts on both arms, and no row gets slower.

## What changed

| design | before | now |
|---|---|---|
| a `constraint` that reads only signals outside the justice literals' cone, and that no infinite path satisfies | `--rung exact` and the default `both`: **violated** (wrong); `l2s` / `kliveness`: holds | every rung: **holds** |
| a constraint that touches the cone (every HWMCC'17 LIVE row measured) | unchanged | unchanged |
| a design with no constraint / fair line | unchanged | unchanged |

The JSON shape is unchanged. Only the `outcome` value moves, and only on the affected shape. The exact cone can get wider on a design whose constraints are disjoint from the property, and the `exact cone: N … bits` note reports the larger number.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | A justice row that was `violated` with the exact engine as decider may now read `holds`, but only when an assumption is disjoint from the property's cone. None of the verdicts we have on record is of that shape. | None. A row that flips is a corrected verdict, not a regression. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | No shape change. | None. |
| **HTTP API agents** | Same as rosf, on `/api/v1/btor2/verify-liveness`. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images | Yes, to get the fix. | A binary change in the cone slicers. |
| rosf `docker/Dockerfile*` | No. | rosf runs the mununu image. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

- Found by the #985 random-BTOR2 justice tier: an explorer draw had k-liveness disagreeing with exact, and the disagreement was exact's fault. The shrunk reproducer is `DISJOINT_DYING_CONSTRAINT` in `crates/mununu-core/src/adapter/justice.rs`. Its test fails on the parent and passes on the fix (`make verify-repro`: CONFIRMED).
- Gate: the 58 rows of the HWMCC'17 LIVE band that carry a constraint or fair line, parent vs fix, 300 s, `--rung both`, in `mununu-sva`. Results: 0 decided-verdict changes, 0 rows that disagree with `live.csv`, 0 slowdowns among rows decided on both arms. Two rows (`lmcs06prodcell1`, `lmcs06prodcell8`) came back `unknown` on the fix while two instances ran side by side. Rerun one at a time, twice each, both arms decide all four runs within 2 s of each other.

## Not covered here

- SystemVerilog designs whose `assume`s lift to `constraint` (verify-auto's liveness and recoverability rows). The fix covers them, because they reach the same slicers, but none was measured.
- Safety (`btor2 verify`): its reach path keeps the constraint already, and a safety version of the reproducer holds on the parent.
