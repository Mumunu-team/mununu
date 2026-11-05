# Consumer briefing — a slice built over a value the trace did not give now says `widened`, not `exact` (2026-11)

> Source of truth: [`SliceReport::no_value`](../../crates/mununu-core/src/adapter/yosys/netlist.rs) + [`TraceNames::attach_slice`](../../crates/mununu-core/src/trace.rs) — surface: CLI+API — the `slice_status` field of a violated row's trace (`sv verify-auto`, `POST /api/v1/sv/verify-auto`; no UI affordance).

**Audience:** rosf, monono and anyone who reads `trace.slice_status` from `sv verify-auto --json` or the API.

## TL;DR

- **A report-field honesty fix; no verdict changes, no JSON shape change.** The trace-relative slice evaluates the design on the counterexample's values. A register or input on the slice's walk that the trace gives **no value** (under any of its names) stays unknown, which keeps every arm it feeds — the slice is wider than the values would make it. Until now `slice_status` still said **`exact`** in that case (mununu#857).
- **Now:** `slice_status` is **`widened`**, and its `reason` gains a clause: `register(s) or input(s) with no value in the trace, left unknown and every arm they fed kept: <names>`. It joins the existing clauses with `; `.
- **"No value" means unknown after evaluation**, not "this name is missing": a register whose alias carries the value is not reported. A register the walk never reaches is not reported either.
- **Measured on the shipped fixture:** the `lane_arbiter` counterexample (the slice's reference design) gives a value to everything its walk reads — its slice stays `exact`. No design has been observed to hit the new clause; it closes a path the failure-path audit of mununu#848 found by reading the code.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a row whose slice crossed a register or input with no value in the trace now shows `slice_status: widened` with the names, where it said `exact`; the `slice` lines are unchanged | none; if you parse `slice_status.reason`, accept the new clause (`; `-joined) |
| **monono** | same as rosf for any `sv verify-auto` row it reads | none; same parsing note |
| **mununu-ui** | none — no UI affordance for the slice | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and read `slice_status` | report-field fix in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow reads `slice_status` | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `sv verify-auto` in the image and read `slice_status` | the fix lives in the binary the volume carries |
| `mununu/docker/Dockerfile.profile` | no | profiling image, slice-independent |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes if rosf pins a mununu version and reads `slice_status` | picks up the fix |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes if monono pins a mununu version and reads `slice_status` | picks up the fix |

## Provenance

Found by `/failure-path-audit` on the mununu#848 change (PR #855) and filed as mununu#857, structural (no design observed to hit it). Reproducer: `crates/mununu-core/tests/slice_missing_value_857.rs` — a register and an input with no trace value each make the status `widened` with the name (both fail on the parent, where the status was `exact`), and a control where every walked value is given stays `exact`.

## Not covered here

Why a trace would omit a register the walk reaches is not diagnosed here — the clause names it so the case is visible when it happens; the cause (a model that did not carry the register as a state cell, or a naming difference between the trace and the flattened netlist) is a follow-up if one is observed.
