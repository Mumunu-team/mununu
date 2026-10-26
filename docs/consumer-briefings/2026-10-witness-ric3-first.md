# Consumer briefing — the counterexample path is found by rIC3's IC3 first and replayed before it ships (2026-10)

> Source of truth: [`bad_reachable_witness_routed`](../../crates/mununu-core/src/adapter/btor2/witness.rs) — surface: (CLI+API) — every VIOLATED safety `trace` of `sv verify-auto` / `POST /api/v1/sv/verify-auto`, `btor2 verify --witness` / `POST /api/v1/btor2/verify` (`witness: true`), and the fault adversary's `attack`

**Audience:** rosf, monono and anyone who reads a `trace` out of a mununu report or gates on its presence. mununu#744 (M7 of the 2026-10-25 plan).

## TL;DR

- **Behaviour change, no new flag.** Where rIC3 is on `PATH` (the `mununu-sva` image; `MUNUNU_RIC3_PATH` otherwise), the counterexample of a VIOLATED safety property is now found by rIC3's bit-level IC3 (`--witness`, under the rescue slice — the engine the portfolio member already runs; rIC3's own BMC is as slow as native BMC on am2901) and **replayed on the concrete simulator** before it ships; native BMC re-derives it only when rIC3 is absent, declines, or its witness does not replay. Without rIC3 nothing changes (`--owned-only` is unchanged).
- **What you may see:** traces on rows that had none — the shallow path native BMC's word-level unrolling did not reach within its bound (am2901: native BMC 900 s and rIC3's BMC still at depth 13 after 120 s → rIC3's IC3 0.2 s) — and, on rows that already had one, a path of the **same depth whose free inputs may be chosen differently** (the states a property forces are the same; the inputs the monitor does not read are a solver's choice either way). A trace is still absent when neither finder returns a replayable path.
- **Verdicts do not move.** The portfolio decides; this explains. `MUNUNU_RESCUE_SLICE_MS=0` disables the member here as everywhere.
- JSON shape, schemas, flags, exit codes: unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | more VIOLATED rows carry a `trace`; a `trace` you already stored may differ in free-input values on re-run | none required; compare traces by `state`, not by byte, if you diff reports |
| **monono** | same; Verilator replay (`--emit-testbench`) works on the new traces as before (they are replayed on the model first) | none required |
| **mununu-ui** | nothing (RTL verbs) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the routing lives in the binary; rIC3 must be on `PATH` for it to engage |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rIC3 ships here; the gates below ran here |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| parser on rIC3's verbatim `--witness` output (and the real am2901 witness as a fixture); replayed trace equals native BMC's states; a lying witness is refused; eight malformed shapes name their defect | 6 unit tests, green |
| first gate run, routing through rIC3 `-e bmc`: the am2901 e2e sat 2 h 22 min in the native fallback — rIC3's BMC times out at 120 s at depth 13 | the lesson that moved the finder to IC3 |
| `e2e_ric3_finds_the_am2901_witness_within_the_slice` (HWMCC'20, `MUNUNU_HWMCC_FLAT`), routing through IC3 | `violated`, a replayed witness of depth 16 in **0.3 s** (native BMC: no answer in 900 s) |
| edn IP e2e on this branch (`mununu-sva`, 2026-10-26) | ok, 1,130 s: 67 properties, 0 unsupported, every violated row carries its trace with `tl_i` at 109 bits |
| edn `run.sh --json` on this branch vs `main` bc82372 (`scratchpad/cex_diff.py`) | **0 verdict moves**; all 11 traces (8 finite, 3 trap) keep their kind and depth (6 and 3); the same 184 state / 4 input / 17 monitor cells per step; the free inputs differ on 1–3 of the 4 per step and the registers they drive diverge accordingly (up to ~45 of 184 by the last step) — a different, replayed path of the same length. The one name difference is pre-existing and unrelated: three `sv2v_cast…$result` register names embed yosys's per-run temp path, so they differ between any two runs (filed separately). |

## Provenance

- Branch `feat/witness-ric3-first`; plan `rosf-rescope-and-claude-config.md` M7; issue #744 (closed as routed, not fixed — native BMC's own search is an engine project, recorded as not built).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The reason a witness could not be produced reaches the log, not the report (`safety_witness` returns no reason) — #803's MEDIUM row.
- rIC3's BMC as a second finder for deep paths; btormc / pono witnesses (same format, same parser — not wired).
- Array states: the replay refuses them; such designs keep native BMC.
