# HWMCC-style coverage study — exact engine vs btormc on a real BTOR2 suite

> Source of truth: [`e2e_hwmcc_style_coverage_study`](../crates/mununu-core/tests/differential_oracle_e2e.rs) + [`exact_bad_reachable`](../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: (test / engine).

## What this measures

The census + parity studies quantify coverage on the **OpenTitan corpus** (models we
build from RTL). This study asks the complementary question on **externally-authored,
real BTOR2 safety benchmarks**: how much of a real benchmark suite does mununu's exact
engine decide, and — the load-bearing part — does every verdict it emits AGREE with an
independent oracle (**btormc**, k-induction)?

Benchmarks are vendored byte-for-byte from **btor2tools** (MIT) — see
[`examples/btor2/btor2tools_suite/PROVENANCE.md`](../examples/btor2/btor2tools_suite/PROVENANCE.md).
The engine bridge is `exact_bad_reachable`: it ORs the design's `bad` nodes and evaluates
`EF(bad)` over the full bit-blasted relation — the exact analogue of btormc's native
bad-reachability.

## Result (9-benchmark suite, mununu-sva)

| | exact engine | btormc | note |
|---|---|---|---|
| count2 / count4 / recount4 / twocount2 | REACHABLE | Violated | ✓ agree |
| factorial4even | REACHABLE | Unknown | multi-property (see below); agree not contradicted |
| twocount32 | Skipped (65 b > cap) | Violated | over the 40-bit cap |
| noninitstate / twocount2c | Refused (`constraint`) | Violated | soundness guard: constraints not modelled |
| ponylink-slaveTXlen-sat | Skipped (2870 b > cap) | Unknown | 320-state real HW; over cap, btormc bounded-out |

- **Coverage:** exact decides **5 / 9** (4 skipped/refused for documented reasons: 2 over
  the bit cap, 2 constraint-refused).
- **Soundness: 0 disagreements** — every verdict the exact engine emits agrees with btormc.
  This is the hard gate; the coverage number is descriptive, not pass/fail.

## What the study CAUGHT (the point of a differential)

The first run flagged `factorial4even`: exact = REACHABLE, btormc = **Safe** — a
contradiction. Investigation vindicated the exact engine (raw `btormc` prints `sat`) and
exposed a **soundness bug in the `run_btormc` wrapper**: `btormc --kind` on a *multi-*`bad`
design reports per property and printed `unsat b1` (property 1 safe) while `b0 = (i==15)` is
reachable — and `parse_btormc_output` read that lone `unsat` as a whole-design SAFE verdict.
A wrapper reporting a reachable design as *safe* is exactly the false-negative a verification
tool must never emit.

**Fix:** `run_btormc` now downgrades a `Safe` parse to `Unknown` when the design has >1 `bad`
(`count_bad_properties`) — a partial proof cannot establish every property safe; a `sat` (some
property reachable) stays a sound `Violated`. That is why `factorial4even` now reads `Unknown`
above (honest) instead of a wrong `Safe`. The module contract doc records the caveat.

## The honest limit

Most real HWMCC benchmarks are 20–200+ latches — far over the 40-bit bit-blast cap (ponylink
is 2870 bits) — so the exact engine `Skip`s them; btormc decides. The study reports that as
coverage, never as a failure. The value is the **soundness cross-check on the decidable
subset**, which found and fixed a real oracle bug.
