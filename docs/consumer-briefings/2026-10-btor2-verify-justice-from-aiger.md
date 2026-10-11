# Consumer briefing — 2026-10 `btor2 verify-justice` + `btor2 from-aiger`: HWMCC-style justice (liveness) sets, and AIGER 1.9 as an input format

> **Audience:** anyone with BTOR2 designs carrying `justice` / `fair` lines, anyone holding AIGER 1.9 benchmarks (HWMCC LIVE sets, `.aig` binary or `.aag`), and the orchestrators that enumerate `btor2` verbs / `/api/v1/btor2/*` routes.
>
> **Related:** the HWMCC'17 LIVE study (benchmarks not vendored — download to `MUNUNU_HWMCC_LIVE_DIR`); `scripts/hwmcc_live_study.sh`. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Two new CLI verbs and one new HTTP route; no change to any existing verdict or wire shape.** `mununu btor2 verify-justice <file>` decides every `justice` line of a BTOR2 file — or of an AIGER 1.9 file, lowered on the fly — with two sound rungs: the `exact-symbolic` fair-cycle fixpoint (Emerson–Lei `E_C G true`, within the bit cap) and liveness-to-safety + the reachability portfolio. `mununu btor2 from-aiger <file.aig>` is the lowering alone. `POST /api/v1/btor2/verify-justice` takes BTOR2 or ASCII AIGER content.

## What changed

- [`adapter/aiger/parser.rs::parse_bytes`](../../crates/mununu-core/src/adapter/aiger/parser.rs): the AIGER adapter reads the **binary** 1.9 format (`aig …`, delta-coded ANDs, justice / fairness / constraint sections, symbol table) besides ASCII; a latch whose init field is its own literal is recorded as **undefined** (free state) instead of being truncated to a byte.
- [`adapter/aiger/to_btor2.rs::circuit_to_btor2`](../../crates/mununu-core/src/adapter/aiger/to_btor2.rs): structural AIGER → BTOR2 lowering (`input`/`state`+`init`+`next`/`and`/`not`/`output`/`bad`/`constraint`/`justice`/`fair`). Measured need: the AIGER toolkit's `aigtobtor` "can not handle latches" and yosys `read_aiger` rejects the binary 1.9 justice sections.
- [`adapter/justice.rs::verify_justice`](../../crates/mununu-core/src/adapter/justice.rs): names the justice / fairness literals as `output`s (`mununu_j<i>_<k>`, `mununu_f<m>`, `mununu_true`, `mununu_false`), then per set: **exact** = `¬E_C G true` with the literals as state fairness assumptions, read universally over the initial states (HOLDS ⇔ no fair path); **l2s** = the response monitor with antecedent `true` / consequent `false` and the literals as fairness atoms, decided by the reachability portfolio. `constraint`s are honoured by both (legal frames / the monitor keeps them).
- CLI: `btor2 verify-justice <FILE> [--rung exact|l2s|both] [--json]` (+ the usual `--fail-on`), `btor2 from-aiger <FILE> [-o OUT]`. API: `POST /api/v1/btor2/verify-justice` `{content, rung?}` → `{verdict, converted_from_aiger, justice: [{index, size, verdict, decided_by, notes}]}`. Binary AIGER cannot travel in a JSON string — convert first.

## Verdict vocabulary

| ours | HWMCC | meaning |
|---|---|---|
| `holds` | `uns` | no infinite path satisfies every constraint forever and every justice + fairness literal infinitely often |
| `violated` | `sat` | such a fair cycle is reachable |
| `unknown` | — | every rung abstained (the exact engine over the cap; the portfolio undecided); `notes` says why |

## Measured

| | result |
|---|---|
| unit: 2-bit counter with `justice {lo ∧ hi}` | VIOLATED by both rungs |
| unit: latch stuck at 1, `justice {¬s}` | HOLDS by both rungs |
| unit: the counter under `constraint ¬lo` (no legal infinite path) | HOLDS by both rungs |
| unit: binary AIGER 1.9 round-trip vs its ASCII twin | identical circuit |
| HWMCC'17 LIVE, ≤192-latch band — 129 instances (124 with ground truth), 300 s each, `mununu-sva` (`scripts/hwmcc_live_study.sh`, 2026-10-11) | 52 decided (13 HOLDS + 39 VIOLATED), **0 disagreements** with the published verdicts; 57 unknown, 20 timeouts. Exact rung 28 (median 0.10 s), l2s + portfolio 24 (median 58 s). No marginal reach over abclive / iimc / nuxmvlive at this budget — the open rows are the exact rung's νμ iterates over the unreachable state space (arena) and long chains (iteration budget); see the PR for the per-instance evidence |

## What to update, per consumer

- **Orchestrators listing `btor2` verbs / routes:** two verbs and one route were added; nothing renamed.
- **Lanes with BTOR2 `justice` lines:** these were previously unreadable as properties (only `bad` was); `verify-justice` decides them. A design without a `justice` line is an error, never a vacuous `holds`.
- **Report parsers:** new JSON shapes only on the new verb/route.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | new verbs/route | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** (only if a lane adopts the verb) |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- adapter::aiger adapter::justice
mununu btor2 from-aiger design.aig -o design.btor2
mununu btor2 verify-justice design.aig --json            # exact first, l2s when it abstains
mununu btor2 verify-justice design.btor2 --rung l2s      # the scalable rung alone
```

## Not covered here

- Word-level `justice` (BTOR2 over bit-vectors wider than 1) goes through the same code; only bit-level sets have been measured.
- The HWMCC LIVE set stays external (download; never vendored).
