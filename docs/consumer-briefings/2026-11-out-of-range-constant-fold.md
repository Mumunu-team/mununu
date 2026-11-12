# Consumer briefing: a property comparing a signal against a constant too wide for it is now evaluated correctly (2026-11)

> Source of truth: [`out_of_range_cmp`](../../crates/mununu-core/src/adapter/btor2/bad_monitor.rs) — surface: CLI+API

**Audience:** anyone whose SVA or `@mununu_guarantee` / `@mununu_template` rows compare a register against a constant that does not fit the register's width, and who reads the results from `sv verify-auto`, `btor2 verify*` or `btor2 verify-liveness`, on the CLI or the HTTP API. The typical shape is an index against a parameter one past its range: `k < N`, with `N = 2**IdxW`.

## TL;DR

- **The defect.** The BTOR2 monitors that Mununu builds for assumptions, the reach and safety rescue, and the l2s liveness rung all used the same construction. They wrote such a constant at the register's width, where it does not fit, and engines then disagreed about the result:
  - native BMC refused the monitor;
  - an engine that reads a constant at its width kept only the low bits, so `k < 8` on a 3-bit `k` read as `k < 0`.
- **What it did to verdicts:**
  - A tautological `assume` (for example OpenTitan `prim_arbiter_ppc`'s `KRange_M`) was reported **contradictory**, which made **every** `holds` on the design vacuous.
  - An `assert` of the same shape could read as `AG false`.
- **Now:** the comparison folds to its fixed answer. The register can never reach the constant, so `==`, `>`, `>=` are false and `!=`, `<`, `<=` are true.
- **Verdicts may change.**
  - A design that reported **contradictory assumptions** may now report consistent ones, and its rows may decide. On `prim_arbiter_ppc`, `NoStarvation_A` held once `KRange_M` was removed by hand; with this fix it is expected to hold with the assume in place, but that has not been re-run yet.
  - A rescue lane that used to abstain on such a row (native BMC's refusal) may now decide it.
- **Unchanged:**
  - the JSON shape;
  - every comparison whose constant fits;
  - the exact engine's own property evaluation, which already compared at the wider width.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Only for rows with an out-of-range constant. Look for a contradictory-assumption note that may disappear, or a row that may move from `unknown` to definite. | Re-run designs whose report named contradictory assumptions. |
| **monono** | Same. | Same. |
| **mununu-ui** | None. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#1012, found on the box on OpenTitan `prim_arbiter_ppc` (FPV harness).
- **Tests:**
  - `a_tautology_over_a_narrow_register_is_not_violated`: native BMC on the emitted monitor. It fails on the parent commit, where native BMC refuses the malformed monitor, and passes on this one.
  - `a_constant_past_the_signals_width_fixes_the_comparison`: the fold table.

## Not covered here

- **The member behind the contradictory reading** on `prim_arbiter_ppc` was not measured. The cause is located in the code.
- **The rest of that harness:** its real lift fails (yosys exit 1), and `prim_arbiter_tree`'s dynamic bit-select rows are unsupported. Both are separate from this fix.
