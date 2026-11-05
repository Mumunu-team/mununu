# Consumer briefing — the counterexample slice no longer reads registers wider than 128 bits through a broken shift (2026-11)

> Source of truth: [`Netlist::evaluate`](../../crates/mununu-core/src/adapter/yosys/netlist.rs) + [`SliceReport::too_wide`](../../crates/mununu-core/src/adapter/yosys/netlist.rs) + [`TraceNames::attach_slice`](../../crates/mununu-core/src/trace.rs) — surface: CLI+API — the `slice` / `slice_status` fields of a violated row's trace (`sv verify-auto`, `POST /api/v1/sv/verify-auto`; no UI affordance).

**Audience:** rosf, monono and anyone who reads `trace.slice` / `trace.slice_status` from `sv verify-auto --json` or the API.

## TL;DR

- **A report-field bugfix; no verdict changes.** The trace-relative slice evaluates the design on the counterexample's values held in 128-bit integers. A register or input wider than 128 bits was read through a shift past bit 127: a **panic** (exit 101, no report) in a debug build, and in a **release build the slice was computed on the wrong bits with no notice** (Rust masks the shift, so bit `i` read bit `i − 128`). Found on the full OpenTitan `edn` IP, a 135-bit cell (mununu#848).
- **Now:** such a register or input stays **unknown** to the evaluator. An unknown keeps every arm of what it feeds, so the slice can only grow, never drop a line that mattered. So does a word cell (`$eq`, `$add`, a shift, …) whose operand is wider than 127 bits, even when the registers feeding it each fit (two 70-bit registers compared as one 140-bit word). When the slice walks through either, `slice_status` is **`widened`** and its `reason` names them (nets by name, word cells by cell name): `net(s) or word operand(s) wider than the evaluator's 128-bit value, left unknown and every arm they fed kept: <names>`. Before, such a word cell left its output unknown with the status still `exact`.
- **Four sibling sites fixed in the same evaluator:** a 127-bit signed operand with its top bit set (overflow in the sign extension), a word result wider than 127 bits (an 8-bit add into a 135-bit net), a shift amount of 2^32 or more (truncated to a shift by 0, a wrong value with no panic), and reading or advancing a net wider than 128 bits as one value.
- **No JSON shape change, no flag or route change.** `slice_status.status` already had `widened`; only its `reason` text gains a clause, and the `slice` lines of an affected row may change (they were computed on wrong bits before).
- **Verdicts are untouched.** The slice is display only; no engine reads it.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a violated row whose slice path crosses a register or input wider than 128 bits now shows `slice_status: widened` with the named nets, and its `slice` lines may differ (the old ones were computed on wrong bits); a debug build no longer exits 101 there | none required; if you parse `slice_status.reason`, accept a `; `-joined reason with more than one clause |
| **monono** | same as rosf for any `sv verify-auto` row it reads; CTXDSL rows carry no slice | none; same `reason` parsing note |
| **mununu-ui** | none — the slice is an RTL report field with no UI affordance | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and read the slice | report-field fix in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow reads the slice | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `sv verify-auto` in the image and read the slice | the fix lives in the binary the volume carries |
| `mununu/docker/Dockerfile.profile` | no | profiling image, slice-independent |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes if rosf pins a mununu version and reads the slice | picks up the fixed evaluator |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes if monono pins a mununu version and reads the slice | picks up the fixed evaluator |

## Provenance

Found on the N1 witness probe of the full `edn` IP (debug build, `mununu-sva`): `attempt to shift right with overflow` at the evaluator's seeding loop after the engines had decided, mununu#848. Reproducers: `crates/mununu-core/tests/netlist_wide_nets_848.rs`, one test per site, each failing on the parent commit (panic or wrong value) and passing on the fix; the report seam is pinned by `a_too_wide_register_on_the_slice_path_reports_a_widened_slice` in `trace.rs`.

## Not covered here

The carrier stays 128 bits: a register wider than that is now honestly unknown rather than evaluated. Carrying the full width (the trace's `hex` already holds it since mununu#723) would make those slices exact again; it is not needed for correctness and is not done here. The `attach_slice` input map still drops a cell whose hex does not fit 128 bits before the evaluator sees it; that drop is now covered by the evaluator's own refusal and reported through `too_wide` whenever the walk reaches the cell.
