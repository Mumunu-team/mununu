# Consumer briefing: the owned SAT BMC gets a dynamic step and writes witnesses (2026-11)

> Source of truth: [`sat_bmc::DynamicStep`](../../crates/mununu-core/src/adapter/btor2/sat_bmc.rs) — surface: CLI-only — engine-evaluation switches of the CLI-only `--bmc-only`; the engine reaches the API when it joins the reach portfolio (Track N, N5.4)

**Audience:** anyone who runs `mununu btor2 verify --bmc-only`.

## TL;DR

- **`--bmc-engine sat-dyn`** is the owned bit-level BMC with a dynamic step (item 6, C1).
  - Each query asks for any bad not yet refuted, at any depth up to a target, under a conflict budget.
  - An UNSAT answer refutes the whole window. A query that runs out of budget refutes nothing, and the next, deeper query covers its depths too.
  - The target depth grows geometrically. This is the lever for a deep counterexample behind hard shallow frames.
- **Counterexample depth.** A counterexample is exact, but it is the earliest bad *in the solver's model* inside an unrefuted window, which is not always the shallowest one overall.
  - `--bmc-engine sat` keeps reporting the shallowest.
  - `NoCounterexample` (outcome `unknown`) is reported only when every depth up to `--bmc-k` was refuted. Otherwise `bmc_stopped` says how far the refutation got.
- **`--bmc-witness-out PATH`** (with `sat` or `sat-dyn`): on a `violated`, writes the counterexample as a BTOR2 witness in the HWMCC format that `btorsim -c` replays.
- **No other changes:** the default path, the portfolio and the API are unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | New values and flag on `--bmc-only` only. | None. |
| **monono** | Same. | None. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the flags. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

Measured 2026-11-14 on HWMCC'20, with a release build, `--timeout-ms 300000`. Every counterexample was replayed by `btorsim -c` on the original file (rc 0), and each witness cut to half its frames was rejected (rc 1).

| row | `--bmc-engine sat-dyn` | replay | reference |
|---|---|---|---|
| `mul9` | violated at depth 126, 252 s | `b0@126` | rIC3 `-e bmc --bmc-dyn-step`: depth 238, 53–62 s; plain SAT BMC stalls at depth 2 |
| `circular_pointer_top_w32_d32_e0` | violated at depth 58, 26 s | `b0@58`, constraints satisfied | rIC3 dyn-step: depth 904, 92 s |
| `krebs.3.prop1-func-interl` | violated at depth 96, 33 s | `b0@96` | plain `--bmc-engine sat`: depth 75, 22 s |
| `circular_pointer_top_w16_d64_e0` | not found in 300 s (refuted up to depth 7) | — | its first bad is at depth 261 (rIC3 on the sticky design) |

**Tests:**
- `the_dynamic_step_reports_the_earliest_true_depth_in_its_window`;
- `the_dynamic_step_refutes_to_the_bound_or_says_it_stopped`;
- `the_witness_parses_and_replays_to_bad` (the repo's own witness parser and concrete replay).

## Not covered here

- **Making it a portfolio member** (`sat-dyn` alongside `ric3-bmc`, then retiring `ric3-bmc`) is N5.4 and needs the box gate first.
- **`circular_pointer_top_w16_d64_e0`** is still decided only through rIC3 on the sticky design.
