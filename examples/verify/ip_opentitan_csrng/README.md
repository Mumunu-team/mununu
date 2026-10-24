# OpenTitan `csrng` — the second full IP through `sv verify-auto`

> Source of truth: [`scripts/vendor_opentitan_ip.py`](../../../scripts/vendor_opentitan_ip.py) (how `source/` was produced) + [`e2e_opentitan_ip_csrng_full`](../../../crates/mununu-core/tests/differential_oracle_e2e.rs) (the pinned verdicts) — surface: CLI-only

The Cryptographically Secure Random Number Generator IP of lowRISC/opentitan at
`558921ccc8aeefab652b45c1402c10bceb63accc`: the TL-UL register block (`csrng_reg_top`), the core, the
main FSM, the command stages, the CTR-DRBG blocks, the state database, two alert senders, the FIFOs and
counters they instantiate — **67 files**, every file byte-exact from upstream
(`source/UPSTREAM_COMMIT.txt` maps each to its path) except the three local files it names: the AES cipher
core is a **black box** (`aes_cipher_core_bbox.sv`, the upstream port list under `(* blackbox *)`, no body:
its outputs are free inputs, a declared over-approximation of the datapath), and two lift-only variants
(below). Vendored 2026-10-22 beside [`ip_opentitan_edn`](../ip_opentitan_edn/README.md); edn's README says
what "one IP at one commit" does and does not show.

## Run it

```bash
# in the mununu-sva image (slang, sv2v, yosys); from this directory
cd source
sh run.sh --json          # the IP's own assertions
```

`run.sh` is `sv verify-auto csrng.sv --top csrng --preprocess-sv2v --source … --lift-override
prim_assert.sv=prim_assert_shim.sv … --pin rst_ni=1`: the SVA is **extracted from the real
`prim_assert` macros**, while the sv2v + yosys lift sees `prim_assert_shim.sv` (every assertion macro of the
real set empty, `INC_ASSERT` defined so the assertion-support logic is in the model) and the two
`_lift.sv` variants, `prim_alert_sender_lift.sv` / `prim_sparse_fsm_flop_lift.sv`, the upstream files with
their `ifdef INC_ASSERT` regions removed because no frontend lifts them (the alert sender's `sequence`
blocks; the sparse-FSM flop's enum-method loop — the removed line ranges are in each file's header).
`rst_ni` is pinned inactive; the TL-UL bus, the entropy source, the three EDN command interfaces and the
life-cycle inputs are **free inputs**.

To re-vendor: `python3 ../../../scripts/vendor_opentitan_ip.py --ip csrng --commit <sha> --checkout <dir>
--out source --blackbox aes_cipher_core --strip-inc-assert prim_alert_sender.sv --strip-inc-assert
prim_sparse_fsm_flop.sv` (needs `slang`; it reports the two `could not resolve hierarchical path name
'u_aes_cipher_control'` diagnostics below as expected).

## What was measured (2026-10-23, `mununu-sva`, shipped defaults)

| rows | holds | violated | unknown (⊥) | unsupported |
|---|---|---|---|---|
| the IP's own SVA (37) | **34** | 0 | 0 | **3** (`gen_aes_cipher_control_fsm_svas`, below) |
| · `ASSERT_KNOWN` (24) | 24 — 20 fold to a constant (the model is 2-state); the four `CsrngGenbits{Fips,Bus}KnownO_A` rows carry an enable and decide on the `explicit` cube | | | |
| · error → alert checks, `ASSERT_PRIM_*_ERROR_TRIGGER_ALERT` (10) | **10** — the four `prim_count` chains (`CntAlertCheck_A` ×3 command stages, `CtrDrbgGenAlertCheck_A`) through the reach rescue's bounded-chain monitor and its rIC3 member; the five FSM rows (`DrbgCmdFsmCheck_A` ×3, `CtrlMainFsmCheck_A`, `DrbgGenFsmCheck_A`) and the register-write one-hot row on the `explicit` cube — the FSM rows **vacuously** (caveats) | | | |

Whole run **110 s**; 216 registers once flattened. `e2e_opentitan_ip_csrng_full` pins every row. The same
34 / 0 / 0 / 3 was measured on 2026-10-18 (ledger §3.3b-6) with the slang frontend forced by hand.

**The lift.** `--frontend auto` tries read_verilog + sv2v first; sv2v's output of this closure is rejected
by yosys (`syntax error, unexpected TOK_DEFAULT`, an unpacked-array default pattern like the one edn
rewrites), so the lift falls back to yosys-slang. That attempt dies in yosys's BTOR backend with
`Found topological loop while processing cell $6638` on a design `check` finds loop-free — yosys-slang
emits 4,035 `$buf` cells and the backend's cell-level walk reads a wide buffer chained through the command
arbiter's grant vector as a cycle — and succeeds on the lift's one retry with `opt_clean` before
`write_btor` (2026-10-23; reported as the `lift-retry` note and `diagnostics.lift_retry`). Both steps are
in the JSON: `diagnostics.frontend`, `frontend_fallback_reason`, `lift_retry`.

## Caveats

- **Three rows are `unsupported` by construction.** `gen_aes_cipher_control_fsm_svas` (csrng.sv:185-191)
  reads `u_aes_cipher_control.u_state_regs.unused_err_o` inside the black-boxed cipher core; there is no
  such hierarchy in the model. They are the only unsupported rows and `e2e_opentitan_ip_csrng_full` pins
  that.
- **The FSM error → alert checks hold with an antecedent the model cannot raise.** The sparse-FSM flop's
  `unused_err_o` is computed only under `INC_ASSERT` by an enum-method loop neither sv2v + yosys nor
  yosys-slang compiles, so the lift uses the file's synthesis branch (`unused_err_o = 1'b0`). The
  `*FsmCheck_A` rows are therefore vacuous HOLDS on this model; under `--fault-model sec-cm` the
  flag is re-minted from the state's compared constants (`docs/verifying-rtl.md` §\"The fault-injection posture\"). `prim_count`'s `fpv_err_present` is in the model and is the
  synthesis constant too (`PrimCountFpv` is not defined); its `err_o` is real logic, so the counter chains
  are not vacuous.
- The black box frees every cipher output: a HOLDS that depends on the cipher's data transfers to the
  RTL (over-approximation), a VIOLATED through a cipher output would not — mununu#751 withholds such a row
  (`violation-over-free-leaves`), and none appears here.
- Budgets as for edn: `MUNUNU_PROPERTY_BUDGET_MS` (15 min) bounds a row, `MUNUNU_RESCUE_SLICE_MS` (60 s)
  each step of the ⊥ re-plan, `MUNUNU_VERIFY_BUDGET_MS` (1 h) the run; keep
  `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` on for a run this size.
