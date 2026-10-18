# OpenTitan `edn` — a full IP through `sv verify-auto`

> Source of truth: [`scripts/vendor_opentitan_ip.py`](../../../scripts/vendor_opentitan_ip.py) (how `source/` was produced) + [`e2e_opentitan_ip_edn_full`](../../../crates/mununu-core/tests/differential_oracle_e2e.rs) (the pinned verdicts) — surface: CLI-only — a vendored example; the same call runs over `POST /api/v1/sv/verify-auto` with the files as `additional_sources`

The Entropy Distribution Network IP of lowRISC/opentitan at `558921ccc8aeefab652b45c1402c10bceb63accc`:
the TL-UL register block (`edn_reg_top`), the core, the main FSM, eight endpoint FSMs
(`gen_ep_blk[k].u_edn_ack_sm_ep`), two alert senders, the FIFOs, counters and arbiter it instantiates —
**58 files, 200 registers once flattened**, every file byte-exact from upstream
(`source/UPSTREAM_COMMIT.txt` maps each to its path) except one documented line (`edn_core.sv`:
`'{default: 1'b0}` → `'{default: '0}`, the same value, which both front ends accept).

This is the first OpenTitan **IP** in the corpus; every other OpenTitan example here is one extracted
block. It answers "block level or IP level?" with numbers — for ONE IP at ONE commit. Nothing below says
"IP scale" in general.

## Run it

```bash
# in the mununu-sva image (slang, sv2v, yosys); from this directory
cd source
sh run.sh --json                                   # the IP's own 52 assertions
sh run.sh --json --source mununu_properties.sv     # + the system-level properties below
```

`run.sh` is `sv verify-auto edn.sv --top edn --preprocess-sv2v --source … --lift-override
prim_assert.sv=prim_assert_shim.sv … --config-value rst_ni=1`: the SVA is **extracted from the real
`prim_assert` macros**, while the sv2v + yosys lift sees `prim_assert_shim.sv`, which defines every macro
of the real set as nothing (as OpenTitan's own synthesis macros do). `rst_ni` is pinned inactive and
every register starts at its reset value; the TL-UL bus, the CSRNG response and the endpoint requests are
**free inputs** — every HOLDS below holds under an adversarial bus, and every VIOLATED is reachable from
reset under one. Set `MUNUNU_PROPERTY_BUDGET_MS` on a run this size (see the caveats).

To re-vendor at another commit: `python3 ../../../scripts/vendor_opentitan_ip.py --ip edn --commit <sha>
--checkout <dir> --out source --rewrite "edn_core.sv|'{default: 1'b0}|'{default: '0}"` (needs `slang`).

## What was measured (2026-10-18, `mununu-sva`, branch `feat/hier-refs-any-instance`, 5-min property budget)

| rows | holds | violated | unknown (⊥) | unsupported |
|---|---|---|---|---|
| the IP's own SVA (52) | **23** | 1 | 20 | 8 |
| · `ASSERT_KNOWN` (13; `$isunknown` folds to false — the model is 2-state) | 13 (`constant`) | | | |
| · error → alert checks, `ASSERT_PRIM_*_ERROR_TRIGGER_ALERT` (15): a fault flag in a sub-instance ⇒ `alert_tx_o[1].alert_p` within 30 cycles | **10** — the main FSM, all 8 endpoint FSMs, the register-write one-hot check (`explicit` cube) | | 5 — the `prim_count` and FIFO-pointer checks (32-bit duplicated counters, cone over the 192-bit cap) | |
| · `EdnFatalAlertNoRsp_A` ×8: `alert[1] |-> edn_o[i].edn_ack == 0` | | **1**, 6 cycles from reset (`symbolic` + BMC rescue) | 7 (the re-plan's BMC runs under a fixed 5-s per-query timeout the property budget never reaches, so one to three of the eight identical rows finish per run, load-dependent — mununu#726) | |
| · `EdnDataStableDisable_A` ×8 (32-bit bus stable while disabled) | | | 8 | |
| · `EdnDataStable_A` ×8 | | | | 8 (`[->1]` goto repetition) |
| system-level properties (`mununu_properties.sv`, 14 rows, below) | **7** | 0 | 7 | — |

Lift (sv2v + yosys): ≈ 10 s. Main pass: ≈ 1 min. The ⊥ re-plan spends the property budget. Engines:
`constant` (atom-less formulas), the `explicit` predicate-cube KMTS (every cross-block invariant), the
`symbolic` cube's BMC rescue (the counterexample); the exact ROBDD engine decided nothing on edn — every
cone it was offered exceeds the 192-bit auto-raised cap (the register block's bus datapath reaches
everything) — while on `pattgen` (87 registers) it decides the four functional assertions.

**The VIOLATED row is a spec gap, not a design bug.** `alert[1]` is the core's combinational
`fatal_alert_o`; the trace walks every endpoint FSM Disabled → EndPointClear → Idle → DataWait → AckPls and
raises a software-triggerable fatal error through the free bus in the cycle endpoint 2 acks. The assertion
has no "no error injection" precondition. Cross-check: SymbiYosys (`read_slang`, `smtbmc yices`) on the same
sources with a reset cycle fails the same assertion for endpoint 3 at step 7 = the reset cycle + mununu's 6; a
deliberately false control (`state_q != Error`) fails at step 4, as it must.

## The system-level properties

`source/mununu_properties.sv` carries `// @mununu_guarantee(<name>) <μ-calculus>` annotations and no
logic. They read the flattened names of the lifted model and the sparse FSM encodings.

| property | reads | verdict |
|---|---|---|
| `edn_main_sm_error_is_terminal` — once in `Error` the main FSM never leaves it | the FSM | **HOLDS** (SymbiYosys `abc pdr` proves it too; `Error` is reachable at step 4, so it is not vacuous) |
| `edn_main_sm_error_is_recorded` / `_record_is_sticky` — `Error` sets the sticky `ERR_CODE` bit next cycle; it never clears | FSM → core → register block | **HOLDS** / **HOLDS** |
| `edn_swport_entry_excludes_boot_mode` — from Idle with `CTRL.BOOT_REQ_MODE` asserted the FSM does not enter SWPortMode | FSM + control register | **HOLDS** |
| `edn_ack_sm_0_error_is_terminal` / `_is_recorded` | endpoint FSM → register block | **HOLDS** / **HOLDS** |
| `edn_fatal_alert_has_a_source` — a fatal alert request is recorded next cycle, or is a FIFO integrity error, or was asked for | core → register block | **⊥** — no verdict in 26 min (the cube's predicate image on a cone over most of the core; not in `mununu_properties.sv`); SymbiYosys `abc pdr` proves the same-cycle form in 3 s — an engine gap, recorded |
| `edn_no_deadlock` — every state has a successor (sanity) | — | **HOLDS** (`constant`) |
| `edn_main_sm_recover_idle` (`AG EF Idle`) and its two `EF` twins | the FSM | ⊥ ×3 (SymbiYosys `cover`: Error and SWPortMode reached at step 4) |
| `edn_ack_sm_{0,7}_recover_idle` and their `EF DataWait` twins | endpoint FSMs | ⊥ ×4 (`cover`: DataWait reached at step 6) |

The invariant "in SWPortMode the boot-request control is off" was written first and is **VIOLATED** —
software may rewrite `CTRL` while the FSM stays in SWPortMode (`edn_main_sm.sv` reads the mode only in
Idle); the property above is the entry-condition form the RTL actually implements. The recoverability rows
are ⊥: the FSM state's cone is over the exact engine's cap and the predicate cube certifies no MUST path;
they are kept as pinned abstentions so an engine improvement breaks the pin loudly. The `EF` twins are true
(SymbiYosys `cover` reaches each target within six cycles of reset), so a HOLDS on the `AG EF` rows would not
be vacuous — the rows are an engine gap, not a property gap. A mutation smoke
(`stick:u_reg.u_err_code_edn_main_sm_err.q`, through the API — `sv mutate` lacks the lift flags, mununu#725;
`e2e_opentitan_ip_edn_stick_flips_the_error_record`): freezing the `ERR_CODE` record flips
`edn_main_sm_error_is_recorded` HOLDS → VIOLATED while `edn_main_sm_error_is_terminal` stays HOLDS — the invariant
reads the record path, not just the FSM.

## Caveats

- One IP, one commit. `csrng` (its AES cipher core black-boxed: 20 HOLDS, all `ASSERT_KNOWN`; its 10
  error → alert checks ⊥) and `pattgen` (14 of 14 HOLDS) lift the same way; their numbers are in the
  benchmark ledger, not pinned here.
- The bus integrity check (`tlul_cmd_intg_chk`) sees a free bus, so an integrity error is reachable from
  cycle 1; a property that mentions the fatal alert must account for it.
- The per-property budget (`MUNUNU_PROPERTY_BUDGET_MS`, 15 min by default) is observed between engine
  calls, not inside a running fixpoint: one annotation (`edn_fatal_alert_has_a_source`, a cone over most
  of the core) ran 26 min without a verdict under a 5-min budget. Keep `MUNUNU_VERIFY_AUTO_PARTIAL_JSON`
  on so the rows already decided survive a kill.
- Counterexample traces list only values that fit 64 bits: the 109-bit `tl_i` is missing from the
  VIOLATED row's trace (mununu#723).
- Multi-clock IPs (`sysrst_ctrl`, `aon_timer`: `prim_reg_cdc`) are not modelled — the lift is single-clock.
