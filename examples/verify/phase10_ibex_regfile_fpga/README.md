# phase10_ibex_regfile_fpga — read-after-write on the ibex register file

> Source of truth: [`encode_design_with_theory`](../../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) + the milestone test [`ibex_regfile_fpga_read_after_write_holds`](../../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) — surface: API (the BvUfArray encoder powers the predicate-cube lift's must-edge SMT queries reachable from `mununu verify`).

> **Status: §Phase 10 / Option-4 step 1d.3 milestone — PASS.** mununu's
> Z3-array encoder verifies the **read-after-write** property of a real
> industrial register file (lowRISC/ibex `ibex_register_file_fpga`),
> cross-checked by an independent `yosys-smtbmc` run. This is a genuine
> verification on real RTL — not a planted demo (per CLAUDE.md
> §Claims Integrity).

## What is verified — and how honestly to read it

**Claim.** For `ibex_register_file_fpga` (RV32E=1, DataWidth=4 → a 16×4
`$mem` array): *when `we_a_i` writes `wdata_a_i` to a nonzero address,
the post-write memory reads that value back at that address.* The Z3
array functional-consistency axiom `read(store(m,A,v),A) == v` forces it.

**Two independent provers agree:**

1. **mununu's encoder** (the automated test). `mununu`'s BTOR2→Z3
   encoder (`encode_design_with_theory(_, Theory::BvUfArray)`, now driven
   through the unified `walk_design::<Z3Backend>` after the Option-4
   step-1c.3 cutover) builds the one-step transition relation with Z3
   array theory and proves the **violation is UNSAT**. Runs in CI (z3 is
   a cargo dependency).
2. **`yosys-smtbmc -s z3`** (the oracle, §10.1 cross-check). An
   *independent* encoding path — Yosys's own RTLIL→SMT-LIB rendering of
   the same RTL, solved by z3 — proves the property holds for a 10-cycle
   bounded check. A divergence would be a soundness bug in mununu's
   btor2→z3 encoder; there is none.

**Scope / honesty.**
- Address 0 is hard-wired to zero in the regfile (R0 read-as-zero), so
  the property is conditioned on `waddr_a_i != 0`, matching the RTL's
  write-enable carve-out.
- The 5-bit address is sliced to the 4-bit array index in *both* the
  read and write paths (Yosys `memory_collect`), so read-after-write
  holds even for nominally out-of-range 5-bit addresses (both map to the
  same 4-bit index).
- The mununu proof is over the **one-step transition relation** (write
  is synchronous, read is asynchronous → the written value is visible in
  the next-step memory). The `yosys-smtbmc` oracle confirms the same over
  a multi-cycle BMC unroll.

## Why this fixture (and not `ibex_register_file_ff`)

`ibex_register_file_ff` — the obvious candidate — is **NOT** suitable:
Yosys reports `"Replacing memory \rf_reg with list of registers"` and
emits **zero array sorts** (it is a genvar loop of individual flip-flops
with one-hot decoded writes). It exercises the plain-BV path, not the
array-memory abstraction.

`ibex_register_file_fpga` uses an addressed RAM
(`mem[waddr_a_i] <= wdata_a_i`), which Yosys infers as a `$mem` → a
BTOR2 array sort → the path this milestone validates. The full
fixture-selection rationale + evidence is in the §10.1 precondition note
(`Phase10-fixture-selection-ibex-regfile-2026-06-13.md`, an untracked note).

## Files

| File | What |
|---|---|
| `emit_btor2.ys` | Yosys script: ibex RTL → 16×4 `$mem` BTOR2 (the checked-in fixture). |
| `ibex_raw_oracle.sv` | Oracle wrapper: remembers a write, drives the read port to that address next cycle, asserts read-after-write (excluding concurrent overwrite). mununu's own; not vendored RTL. |
| `oracle.ys` | Yosys script: wrapper + RTL → SMT-LIB for `yosys-smtbmc`. |
| `../../../crates/mununu-core/tests/data/ibex_register_file_fpga_16x4.btor2` | The generated BTOR2 fixture the mununu test consumes (a generated artifact — the ibex RTL itself is NOT vendored, per §10.3). |

## Reproduce

The ibex RTL is pulled at reproduction time (Apache-2.0), not vendored:

```bash
cd examples/verify/phase10_ibex_regfile_fpga
curl -sSL -o ibex_register_file_fpga.sv \
  https://raw.githubusercontent.com/lowRISC/ibex/master/rtl/ibex_register_file_fpga.sv

# (a) regenerate the BTOR2 fixture the mununu test consumes
yosys -q emit_btor2.ys            # → ibex_register_file_fpga_16x4.btor2 (1 array sort)

# (b) mununu's encoder proves read-after-write (violation UNSAT)
cargo test -p mununu-core --lib ibex_regfile_fpga_read_after_write_holds

# (c) independent oracle: yosys-smtbmc + z3 (PASSED, 10 cycles)
yosys -q oracle.ys                # → ibex_raw_oracle.smt2
yosys-smtbmc -s z3 -t 10 ibex_raw_oracle.smt2   # → Status: PASSED
```

(SBY is not required — `yosys-smtbmc` is its BMC backend; only `yosys` +
`z3` are needed for the oracle.)
