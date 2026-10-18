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

## What was measured (2026-10-18, `mununu-sva`, shipped defaults — `MUNUNU_PROPERTY_BUDGET_MS` 15 min, `MUNUNU_RESCUE_SLICE_MS` 60 s)

| rows | holds | violated | unknown (⊥) | unsupported |
|---|---|---|---|---|
| the IP's own SVA (52) | **39** | **8** | 5 | 0 |
| · `ASSERT_KNOWN` (13; `$isunknown` folds to false — the model is 2-state) | 13 (`constant`) | | | |
| · error → alert checks, `ASSERT_PRIM_*_ERROR_TRIGGER_ALERT` (15): a fault flag in a sub-instance ⇒ `alert_tx_o[1].alert_p` within 30 cycles | **10** — the main FSM, all 8 endpoint FSMs, the register-write one-hot check (`explicit` cube) | | 5 — the `prim_count` and FIFO-pointer checks (32-bit duplicated counters): since the bounded-chain monitor (2026-10-18) they reach the reach portfolio and stay ⊥ with the 60-s rescue slice spent (`budget-expired`) — the proof needs the invariant "the two counter copies agree", which k-induction cannot find from a free step state and z3 SPACER does not find within the slice; SymbiYosys `abc pdr` proves `!err_o` in 8 s | |
| · `EdnFatalAlertNoRsp_A` ×8: `alert[1] |-> edn_o[i].edn_ack == 0` | | **8**, each 6 cycles from reset with its trace (the reach rescue's native BMC, 16–33 s a row under the 60-s `MUNUNU_RESCUE_SLICE_MS`; before mununu#726 the rescue ran under a fixed 5-s query timeout and one to three rows decided per run) | | |
| · `EdnDataStableDisable_A` ×8 (`!enable |=> ##1 $stable(bus)`, 32-bit bus stable while disabled) | **8** — the reach rescue's bounded-chain monitor, native k-induction, 1–2 s a row (2026-10-18) | | | |
| · `EdnDataStable_A` ×8 (`$stable(bus) throughout (req \|\| !enable)[->1]`, lowered as a weak-until since 2026-10-18) | **8** — the same monitor with the weak-until block judged by a pending latch, native k-induction, 3–5 s a row. SymbiYosys `abc pdr` proves both families for all eight endpoints in 18 s; a deliberately false control over the same pending latch fails at step 8 (the obligation is exercised) | | | |
| system-level properties (`mununu_properties.sv`, 15 rows, below) | **12** | 0 | 3 | — |

Total: 67 properties, **51 HOLDS / 8 VIOLATED / 8 ⊥ / 0 unsupported**; `e2e_opentitan_ip_edn_full` pins them in 1 486 s (the IP's own 52 rows alone run in 966 s under `run.sh`; the eight ⊥ rows are the five fault-flag chains and the three `AG EF` rows, each paying one 60-s rescue slice).

Lift (sv2v + yosys): ≈ 10 s. Main pass: ≈ 1 min per engine. The ⊥ re-plan runs once on the merged report and spends at most the 60-s rescue slice per undecided row. Engines:
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
| `edn_fatal_alert_has_a_source` — a fatal alert request is recorded next cycle, or is a FIFO integrity error, or was asked for | core → register block | **VIOLATED in 7 s** (reach rescue) — and rightly so: the first form listed six of the eight `ERR_CODE` bits; the trace is a software `ERR_CODE_TEST` write that records `sfifo_rescmd_err`. Before the budget plumbing of mununu#726/#728 this row ran 26 min without a verdict. The corrected form (all eight `ERR_CODE` bits, the one in `mununu_properties.sv`) **HOLDS in 0.5 s** by the same rescue (k-induction) — the fact SymbiYosys's PDR proved |
| `edn_no_deadlock` — every state has a successor (sanity) | — | **HOLDS** (`constant`) |
| `edn_main_sm_recover_idle` (`AG EF Idle`) | the FSM | ⊥ |
| its two `EF` twins (`Error`, `SWPortMode` reachable) | the FSM | **HOLDS** ×2 — decided on the dual by the reach rescue (SymbiYosys `cover` reaches both at step 4) |
| `edn_ack_sm_{0,7}_recover_idle` | endpoint FSMs | ⊥ ×2 |
| their `EF DataWait` twins | endpoint FSMs | **HOLDS** ×2 (`cover`: step 6) |

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
- Budgets: `MUNUNU_PROPERTY_BUDGET_MS` (15 min) bounds a row, `MUNUNU_RESCUE_SLICE_MS` (60 s) each
  step of the ⊥ re-plan, `MUNUNU_VERIFY_BUDGET_MS` (1 h) the run; a row that hits its budget carries
  `bottom_reason: budget-expired` and the rows after it are still attempted (mununu#726, #728 — before
  them the fatal-alert invariant ran 26 min under a 5-min budget). Keep `MUNUNU_VERIFY_AUTO_PARTIAL_JSON`
  on so the rows already decided survive a kill from outside.
- Counterexample traces list only values that fit 64 bits: the 109-bit `tl_i` is missing from the
  VIOLATED row's trace (mununu#723).
- Multi-clock IPs (`sysrst_ctrl`, `aon_timer`: `prim_reg_cdc`) are not modelled — the lift is single-clock.
