# Consumer briefing: the native engines encode division, remainder and the overflow predicates (2026-11)

> Source of truth: [`encode_op`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) — surface: CLI+API — every verb whose native BMC / k-induction or interpolation member reads BTOR2 (`btor2 verify`, `--owned-only`, `--bmc-only`, `sv verify-auto`, the liveness rungs) on the CLI and the HTTP API

**Audience:** anyone who reads a reach-portfolio verdict or its `refused` list on designs with `udiv`, `urem`, `sdiv`, `srem`, `smod` or an overflow predicate (`uaddo`, `usubo`, `saddo`, `ssubo`, `umulo`, `smulo`, `sdivo`).

## TL;DR

- **The z3 encoder takes these ops now (mununu#1037).** Until now it refused them, so the native BMC / k-induction and interpolation members refused every design that had one.
  - Division and remainder map to z3's SMT-LIB primitives. BTOR2's semantics are SMT-LIB's, including at a zero divisor: `udiv x 0 = ~0`, `urem x 0 = x`.
  - The overflow predicates are extend-and-compare, with the same definitions as the AIG backend's.
- **What you'll see:**
  - The refusal `division/modulo (sdiv/udiv/smod/srem/urem)` or `overflow detector` no longer appears in `refused` for `native`, `cex` or `interp`.
  - Those members may now decide rows the portfolio used to leave `unknown`. `--bmc-only` with the default `--bmc-engine native` returns a verdict instead of exiting 1.
- **No verdict changes value:** the encoding is exact, and a disagreement between members still raises the contradiction alarm. **No JSON shape change.**
- **Unchanged:** the concrete simulator still refuses these ops. A predicate-cube / KMTS lift over such a design still refuses, now at the sampling step, and a witness replay through the simulator still fails on them.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Rows with these ops may decide where they were `unknown`. | None. |
| **monono** | Same. | None. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the encoding. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the N5.0 gate on HWMCC'20. `anderson.3.prop1-back-serstep` and `elevator.4.prop1-func-interl` exited 1 in 18 ms under native `--bmc-only`.
- **End to end** (this branch, release, laptop host):
  - `anderson.3`, native `--bmc-only --bmc-k 100`: `violated` at depth 3 in under 1 s, exit 2 (it was exit 1 with the refusal).
  - `anderson.3`, `--owned-only --timeout-ms 120000`: `violated` by `native` and `cex`.
  - `elevator.4`, `--owned-only --timeout-ms 120000`: `unknown`. No member refuses on division any more: `cex` searched to depth 28, `native` was still running at the deadline, and `interp` was undecided. `elevator.4` is SAT-decided by the `sat-bmc` member (#1036), which this branch predates.
  - `elevator.4`, native `--bmc-only --bmc-k 100 --timeout-ms 300000`: had not returned after 2 h 51 min, because `--timeout-ms` does not bound native BMC there (#1039).
- **Tests:**
  - `division_and_overflow_encode_to_their_smtlib_definitions` (proptest, widths 1–8, against the SMT-LIB reference shared with the AIG backend);
  - `division_edge_cases_encode_exactly` (zero divisors, `INT_MIN / -1`, 1-bit `sdivo`, signed `smod` / `srem`).

## Not covered here

- The exact BDD bit-blaster still refuses `srem` and the other division ops (`operator Srem not yet supported in the R-F5.3a BDD bit-blaster`); that refusal is reported, not silent.
- Teaching the concrete simulator these ops, which would unblock the predicate-cube lift and witness replay on such designs.
