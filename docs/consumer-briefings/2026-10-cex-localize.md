# Consumer briefing — repair candidates under a counterexample (`sv verify-auto --localize`, 2026-10)

> Source of truth: [`repair::localize`](../../crates/mununu-core/src/adapter/btor2/repair.rs) + [`attach_repair_candidates`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`AutoVerifyReport::trace_of`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto --localize`, `POST /api/v1/sv/verify-auto` with `localize: true`.

**Audience:** rosf, monono, mununu-ui and anyone who reads a `sv verify-auto` counterexample (text or the `Trace` JSON).

## TL;DR

- **New, opt-in.** `--localize` (API `localize: true`) asks, for every violated row with a finite counterexample: with this trace's inputs fixed, which register's next-state function, if free at some cycles, would let the property survive the whole trace? Each such register is a **repair candidate** — the place a one-register change could remove this counterexample. Printed under the counterexample as `repair candidates:`; in the JSON as `trace.repair_candidates` and `trace.localization`. Off by default: one native-BMC query per register in the property's cone (times up to four override budgets).
- **Candidates, never causes.** A register UPSTREAM of the property can repair by steering the design elsewhere: on the fixture, forcing the stall counter to its limit at reset trips the fault arm one cycle later, the arbiter parks in FAULT and the second grant never happens — a correct answer to the question asked that is not where the bug is. Candidates are therefore **ranked by how close to the violation their change acts** (the state register's change at frame 1 — the state entering the second request — first, the stall counter's at reset after), and a register the property reads directly (overriding what is measured "repairs" anything) is reported last, in its own bucket (`reads_property`).
- **Minimal overrides, latest first.** Budgets are tried in order — one frame's change, two, three, unbounded — and for one frame the frames are tried from the last backwards, so the report names the LATEST single-frame change that still avoids the violation (deterministic) rather than a scatter of harmless overrides. On the fixture that is frame 1 for the state register, not frame 2: the `start_a` pulse is computed from the combinational `next`, so overriding what the register takes at frame 3 cannot stop it, while any frame-2 state other than IDLE does.
- **Status is always stated.** `done`, `partial` (a cap of 64 candidates, or a query that could not run — the reason names it), `none` (no single-register change avoids the violation within the trace), `unavailable` (a lasso / liveness counterexample has no finite length to fix; a row whose shape has no `bad` monitor; no input sequence recorded).
- **No verdict changes.** Display only; no engine reads it.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `Trace` gains optional `repair_candidates: [{register, cycles, values, lines, reads_property}]` and `localization: {status, reason?}`, present only when `localize` was requested; the request gains optional `localize` (default false); text output gains the `repair candidates:` block | none required; opt in per run when a human will read the row |
| **monono** | same | none required |
| **mununu-ui** | `Trace` gains the two optional fields | optional |

## JSON shape (schemas regenerated)

| field | where | when present | meaning |
|---|---|---|---|
| `localize` | `SvVerifyAutoRequest` | — | `bool`, default `false` |
| `repair_candidates` | `trace` | `localize` ran and found at least one | `[{register, cycles: [t…], values: [hex…], lines: [{file, line}], reads_property}]`, ranked: the ones the property does not read first, closest-acting first |
| `localization` | `trace` | `localize` ran on the row | `{status: "done" \| "partial" \| "none" \| "unavailable", reason?}` |

## Text output

```
        repair candidates (with the trace's inputs fixed, a change here avoids the violation within 4 cycles):
          state_q at [1] := 0x3: lane_arbiter.sv:47, 52
          stall_q at [0] := 0x4: lane_arbiter.sv:48, 55-56
          start_a — reads the property: a change here changes what is measured, not what happens
          start_b — reads the property: a change here changes what is measured, not what happens
```

`[t] := v` is the override's frame and value; the lines are the statements that write the register. A non-`done` status prints once: `localization: none — no single-register change among 4 avoids the violation within the trace's 4 cycles`.

## Engine

**engine:** native BMC (in-process Z3, QF_BV; `native_bmc::bmc_bad_reachable_witness`), depth = the trace's last frame (where the monitor's `bad` fired; a frame beyond it would have free inputs, and a violation they cause is not this one), one query per candidate register and override budget; structure: the design with the row's violation as a `bad` (the same safety witness monitor the counterexample path came from), the fault posture's override mux on the register's `next`, a cycle counter and one `constraint (cycle != t) || (in == v_t)` per design input and frame, the `bad`s folded into a `seen` latch; role: decider of the repair-existence query. Not a verdict lever; the wall-class matrix is not the instrument.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the query builder lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the e2e below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |

## Provenance

Branch `feat/cex-localize`. Fixture `crates/mununu-core/src/adapter/btor2/testdata/lane_arbiter.{sv,shadow.btor}` (the slice fixture: a first-draft sequencer whose alternation guarantee is violated; the BTOR2 the 0.5.8 engine ran). Host: `repair::tests` — the pinned frames replay the trace (the monitored model alone violates at depth 3; every design input of frames 0–3 in the query's witness equals the trace's), `state_q` is a candidate at budget 1, latest frame 1, any state but LANE_A (frame 2 cannot be it: the pulse reads `next`); `stall_q` is a candidate at budget 1, frame 0, value 4 (the fault diversion — a wrong prediction of mine, corrected by the measurement); a site that is not a register, a model without `bad` and an empty trace are refused. Image: `e2e_localize_names_the_state_register_at_the_second_grant_first` — passes — `state_q` first at [1] := 0x3 (lines 47, 52: the reset branch and `state_q <= next`), then `stall_q` at [0] := 0x4 (lines 48, 55–56), then `start_a` at [2] := 0x0 and `start_b` at [2] := 0x1 as reads-the-property; status `done`.

## Not covered here

Statement-level refinement of a candidate's lines through the trace-relative slice (#838 / #839: once merged, the lines at the repair cycle that write the register); `btor2 verify --localize` on a BTOR2 given directly; multi-register repairs (pairs) — the status says `none` when no single register repairs.
