# Consumer briefing — the trace-relative slice under every `sv verify-auto` counterexample (`slice:`, 2026-10)

> Source of truth: [`Netlist::slice`](../../crates/mununu-core/src/adapter/yosys/netlist.rs) + [`TraceNames::attach_slice`](../../crates/mununu-core/src/trace.rs) + [`PropertyVerdict::trace`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — every `sv verify-auto` row with a counterexample, and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono, mununu-ui and anyone who reads a `sv verify-auto` counterexample (text or the `Trace` JSON) or renders one.

## TL;DR

- **Additive.** Every RTL counterexample now carries, after its `cells:` legend, a `slice:` block: per cycle, the RTL lines that **decided** the values the property read at that step — the branch taken and the operands that controlled it, nothing else. `involves:` (the cone of influence, 2026-10-30) is the code the verdict is *about*; the slice is the code that *ran* on this path: the cone's statements that ran, plus the branch headers (`case`, `if`) that selected them — a `case (x)` line writes nothing, so the cone does not list it, and the slice does.
- **Never smaller than the truth.** The slice is computed by evaluating the flattened netlist (the same yosys `write_json` the legend reads) on each step's values and walking backwards with the controlling-value rule (a `$mux`/`$pmux` contributes its selector and the selected input only; an `and` that is 0 the operands that are 0; an `or` that is 1 the operands that are 1). A cell type the evaluator does not know keeps its whole fan-in and the slice says `widened`, naming the type. Cross-checked against the engine's own one-step evaluator on the lifted BTOR2 (unit test) and against the trace itself (every register's `D` at `t` equals the trace's state at `t+1`).
- **Status is always stated.** `exact`, `widened — <cell types>`, or `unavailable — <reason>` (the sv2v front end rewrote the source so its lines are not the RTL's; no netlist because the model did not come from the SystemVerilog lift; the property reads no public net). Nothing is printed as an empty block.
- **No verdict changes.** Display only; no engine reads the slice.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `Trace` (the counterexample object of every verify-auto row, and the `btor2` verbs' witnesses) gains optional `slice: [{cycle, lines: [{file, line, text?}]}]` and `slice_status: {status, reason?}`; text output gains the `slice:` block under a counterexample | none required; optional: show the slice lines to the engineer who reads the row — they point at the statements, not the cone |
| **monono** | same JSON additions | none required |
| **mununu-ui** | `Trace` gains the two optional fields; `TraceView` may render `slice` as a per-cycle list under the legend (the `cells:` legend PR #82 already renders `trace.sources`) | optional; nothing breaks without it |

## JSON shape (`sv-verify-auto-response.schema.json`, regenerated)

| field | where | when present | meaning |
|---|---|---|---|
| `slice` | `trace` | the slice has at least one line | `[{cycle, lines: [{file, line, text}]}]` — `cycle` is the step whose transition the lines decided; `text` is the line as written, trimmed, ≤ 72 chars, when the run holds the source |
| `slice_status` | `trace` | a slice was attempted (every RTL trace) | `{status: "exact" \| "widened" \| "unavailable", reason?}` |

Both are additive and optional; a CTXDSL path (named states) carries neither.

## Text output

```
        cells:
          state_q ← lane_arbiter.sv:26 (register, assigned at lane_arbiter.sv:45)
          …
        slice:
          [0] lane_arbiter.sv:34  case (state_q)
          [0] lane_arbiter.sv:35  IDLE:           if (req) next = LANE_A;   // the first draft always grants A
          …
          [2] lane_arbiter.sv:53  start_a <= (next == LANE_A) && (state_q != LANE_A);
```

A non-exact status prints once under the block: `slice: widened — cell type(s) the evaluator does not know, their whole fan-in kept: $shiftx`.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the evaluator lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the e2e below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |

## Provenance

Branch `feat/trace-slice`. Fixture: `crates/mununu-core/src/adapter/yosys/testdata/lane_arbiter.{sv,flat.json,shadow.btor}` — a first-draft sequencer whose alternation guarantee is violated (the cone of its `alternates` row is 24 lines; the slice is four to six per cycle), its netlist from the lift's own yosys script in `mununu-sva`, the BTOR2 the engine ran (`MUNUNU_SHADOW_BTOR2_DUMP`) and the counterexample the 0.5.8 release binary reported. Host: the netlist evaluator's seven unit tests (parse; step-by-step agreement with the measured trace; the differential against `bit_blast::simulate_one_step` on the engine's BTOR2; the slice's line sets per cycle; the controlling-value rule on minimal netlists; an unknown cell widens; an unknown seed is reported). Image: `e2e_the_slice_of_a_counterexample_names_the_lines_that_decided_it` — passes under the CLI's default engine selection — status `exact`, four steps, the slice per cycle `[0]` 34, 35, 45, 53, 54, 55 · `[1]` 34, 37, 38, 45, 53, 54 · `[2]` 34, 35, 45, 53, 54 · `[3]` 45 (the flop that produced the last state), the line text attached (`IDLE:           if (req) next = LANE_A;   // …`); lines 40–41 in the cone and not in the slice, 60–63 in neither, and slice − cone = {34} (the `case` header).

## Not covered here

Repair-candidate localisation (`--localize`, the next PR of the same plan); slices for `btor2 verify` witnesses on a BTOR2 given directly (no netlist: `unavailable`, by design).
