# Consumer briefing — a VIOLATED that reads a clock as data is withheld (2026-11)

> Source of truth: [`withhold_violated_over_free_leaves`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`BottomReason::ViolationOverClockAsData`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`, every `violated` row

**Audience:** rosf, monono, mununu-ui, and anyone whose gate counts `violated` rows of `sv verify-auto` on designs with latches or other logic that reads a clock level. Fixes mununu#1030.

## TL;DR

- **A `violated` row can become `unknown`.** The model steps once per clock edge. Logic that reads the clock's *level*, typically a latch enabled by the clock (`always_latch if (!clk) l = d;`), lifts with the clock as a free input, unrelated to those edges. A `holds` over that model transfers to the hardware. A `violated` may need a clock value the hardware never shows between edges. On the issue's design, a property that holds on the hardware shipped `violated`, with no note.
- **Now:** a `violated` row whose cone reads a clock net as data becomes **`unknown`**, with `bottom_reason.kind = "violation-over-clock-as-data"`. `detail` names the clocks, and the trace is **kept** in `counterexample` as a may-trace.
- **Unchanged:** `holds`; a refuted `EF p`; and every row whose cone does not read a clock net. A latch enabled by a data signal is unaffected: it lifts correctly.
- **Schema:** `bottom_reason.kind` has one more value (the description table in `sv-verify-auto-response.schema.json`). No field is added or removed.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`) | a `violated` row through a clock-read latch reads `unknown` (`violation-over-clock-as-data`); under `--fail-on unknown` it still fails the gate, under the default it no longer does | rerun; a row that moved is one whose earlier `violated` was not established |
| **monono** (direct CLI `--json`) | same | rerun the lane at the next pin |
| **mununu-ui** | a new `bottom_reason.kind` value renders like the others | none required (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the post-pass lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not decide verdicts |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | `violated` over a clock read as data → `unknown` |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured (mununu-sva)

| run | before | after |
|---|---|---|
| `e2e_a_latch_enabled_by_the_clock_level_withholds_its_violation` (the issue's design + a data-enabled latch control) | `Follows_A` (holds on the hardware) **`violated`**, no note (fails: `a clock read as data must not ship a definite VIOLATED: Violated { false_cells: 0 } None`) | `Follows_A` **`unknown`**, `violation-over-clock-as-data` naming `clk`, trace kept; control rows unchanged (`Transparent_A`, `Holds_A` holds; `Frozen_A` violated) — PASS |
| `e2e_opentitan_sva_census` | 191 rows: 173 holds · 11 skipped · 7 violated (main `fb4b17cd`) | 191 rows: 173 holds · 11 skipped · 7 violated — **identical row for row**; no corpus row's cone reads a clock as data |

## Verification steps for every consumer

```bash
# a design with a latch transparent while clk is low, feeding a posedge-clk flop:
# expect unknown, bottom_reason.kind = violation-over-clock-as-data, the trace still in counterexample
mununu sv verify-auto top.sv --top top --json | jq '.properties[] | {name, outcome, k: .bottom_reason.kind}'
```

## Provenance

- Fix commit: pending merge — branch `fix/clock-as-data-1030`.
- Issue: mununu#1030.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- A faithful clock-level model, e.g. `clk2fflogic`, under which such a latch would decide (roadmap B6).
- `$adlatch` / `$dlatchsr` and a latch-based clock gate (`gclk = clk & l`) were not measured separately. The withholding keys on the clock net in the cone, not on the cell type, so it covers them if their lift reads the clock as an input.
