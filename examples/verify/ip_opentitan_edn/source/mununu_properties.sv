// System-level properties for OpenTitan `edn` (lowRISC/opentitan 558921c), checked by `sv verify-auto`
// beside the IP's own SVA (`sh run.sh --json --source mununu_properties.sv`). This file carries no logic:
// the empty package keeps it a legal compilation unit for slang / sv2v / yosys (an empty MODULE would be
// reported as a black box); the properties are the `@mununu_guarantee` annotations below, scanned from
// every `--source` file.
//
// Names are the flattened ones of the lifted model (`MUNUNU_SHADOW_BTOR2_DUMP` lists them; a `for`-generate
// iteration is spelled `gen_ep_blk$k`); state encodings are the sparse ones of `edn_pkg::state_e`
// (main FSM: Idle 193, SWPortMode 149, RejectCsrngEntropy 24, Error 382) and `edn_ack_sm::state_e`
// (Disabled 306, EndPointClear 398, Idle 92, DataWait 235, AckPls 37, Error 465); MuBi4 True is 6.
// The verdicts (2026-10-18, mununu-sva) are in ../README.md; `e2e_opentitan_ip_edn_full` pins them.
package mununu_props_edn; endpackage

// --- the main FSM: its SEC_CM trap is terminal, recorded, and the record is sticky -----------------
// Once in Error the FSM never leaves it (until reset).
// @mununu_guarantee(edn_main_sm_error_is_terminal) nu X. ((!(u_edn_core.u_edn_main_sm.state_q == 382) || nu Y. ((u_edn_core.u_edn_main_sm.state_q == 382) && [] Y)) && [] X)
// Error is recorded in the sticky ERR_CODE register one cycle later (FSM → core → register block) …
// @mununu_guarantee(edn_main_sm_error_is_recorded) nu X. ((!(u_edn_core.u_edn_main_sm.state_q == 382) || [] u_reg.u_err_code_edn_main_sm_err.q) && [] X)
// … and the record never clears.
// @mununu_guarantee(edn_main_sm_error_record_is_sticky) nu X. ((!(u_reg.u_err_code_edn_main_sm_err.q) || [] u_reg.u_err_code_edn_main_sm_err.q) && [] X)
// From Idle with boot-request mode asserted, the FSM does not enter SWPortMode next cycle. (The invariant
// form "in SWPortMode the boot-request control is off" is FALSE: software may rewrite CTRL while the FSM
// stays in SWPortMode — the exclusion is an entry condition, `edn_main_sm.sv` reads the mode only in Idle.)
// @mununu_guarantee(edn_swport_entry_excludes_boot_mode) nu X. ((!(u_edn_core.u_edn_main_sm.state_q == 193 && u_reg.u_ctrl_boot_req_mode.q == 6) || [] !(u_edn_core.u_edn_main_sm.state_q == 149)) && [] X)

// --- the fatal alert path (core → register block) ------------------------------------------------
// A fatal alert request from the core is recorded in some ERR_CODE bit the next cycle, or it is a FIFO
// integrity error, or software asked for it (ALERT_TEST). All eight ERR_CODE bits are listed: a first
// form that named six of them was VIOLATED — the trace was a software ERR_CODE_TEST write recording
// `sfifo_rescmd_err` — which is how the missing two were found.
// @mununu_guarantee(edn_fatal_alert_has_a_source) nu X. ((!(u_edn_core.fatal_alert_o) || u_edn_core.sfifo_rescmd_int_err || u_edn_core.sfifo_gencmd_int_err || u_reg.u_alert_test_fatal_alert.q || [] (u_reg.u_err_code_sfifo_rescmd_err.q || u_reg.u_err_code_sfifo_gencmd_err.q || u_reg.u_err_code_edn_main_sm_err.q || u_reg.u_err_code_edn_ack_sm_err.q || u_reg.u_err_code_edn_cntr_err.q || u_reg.u_err_code_fifo_write_err.q || u_reg.u_err_code_fifo_read_err.q || u_reg.u_err_code_fifo_state_err.q)) && [] X)

// --- the endpoint FSMs (8 × edn_ack_sm): the same trap discipline on endpoint 0 ----------------------
// @mununu_guarantee(edn_ack_sm_0_error_is_terminal) nu X. ((!(u_edn_core.gen_ep_blk$0.u_edn_ack_sm_ep.state_q == 465) || nu Y. ((u_edn_core.gen_ep_blk$0.u_edn_ack_sm_ep.state_q == 465) && [] Y)) && [] X)
// @mununu_guarantee(edn_ack_sm_0_error_is_recorded) nu X. ((!(u_edn_core.gen_ep_blk$0.u_edn_ack_sm_ep.state_q == 465) || [] u_reg.u_err_code_edn_ack_sm_err.q) && [] X)

// --- recoverability (measured ⊥ — kept as pinned abstentions, see the README) ----------------------
// Every reachable state of the main FSM can get back to Idle (AG EF), with the EF twins that would keep a
// HOLDS non-vacuous (Idle is left: Error and SWPortMode are reachable).
// @mununu_guarantee(edn_main_sm_recover_idle) nu Y. ((mu X. ((u_edn_core.u_edn_main_sm.state_q == 193) || <> X)) && [] Y)
// @mununu_guarantee(edn_main_sm_error_reachable) mu Z. ((u_edn_core.u_edn_main_sm.state_q == 382) || <> Z)
// @mununu_guarantee(edn_main_sm_swport_reachable) mu Z. ((u_edn_core.u_edn_main_sm.state_q == 149) || <> Z)
// Each endpoint FSM can always get back to Idle (software can disable and re-enable) and does leave it.
// @mununu_guarantee(edn_ack_sm_0_recover_idle) nu Y. ((mu X. ((u_edn_core.gen_ep_blk$0.u_edn_ack_sm_ep.state_q == 92) || <> X)) && [] Y)
// @mununu_guarantee(edn_ack_sm_0_datawait_reachable) mu Z. ((u_edn_core.gen_ep_blk$0.u_edn_ack_sm_ep.state_q == 235) || <> Z)
// @mununu_guarantee(edn_ack_sm_7_recover_idle) nu Y. ((mu X. ((u_edn_core.gen_ep_blk$7.u_edn_ack_sm_ep.state_q == 92) || <> X)) && [] Y)
// @mununu_guarantee(edn_ack_sm_7_datawait_reachable) mu Z. ((u_edn_core.gen_ep_blk$7.u_edn_ack_sm_ep.state_q == 235) || <> Z)

// --- sanity: every state has a successor (a BTOR2 transition relation is total; decided as a constant) --
// @mununu_guarantee(edn_no_deadlock) nu X. (<> true && [] X)
