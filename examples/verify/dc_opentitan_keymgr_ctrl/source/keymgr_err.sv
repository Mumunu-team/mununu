// Stub of lowRISC OpenTitan `keymgr_err` standing in for a module this corpus entry never carried
// (port list from `hw/ip/keymgr/rtl/keymgr_err.sv` at 558921cc, Apache-2.0 — see UPSTREAM_COMMIT.txt). sv-elab refuses an
// instantiation with no definition (2026-11-04), where `read_verilog` lifted it with the cell
// undefined and its outputs anonymous free inputs. `(* mununu_missing_module *)` restores exactly
// that: the lift keeps the (empty, hence black-box) module only through the hierarchy snapshot —
// which names it in `diagnostics.blackboxed_modules` — and deletes it before the BTOR2 is written,
// so the cell is undefined again and its outputs are free, non-deliberate leaves: a VIOLATED over
// them is withheld as `violation-over-free-leaves` (mununu#750), as before. Without the attribute
// an empty module is a DELIBERATE `(* blackbox *)` cut that keeps its verdict. SOUNDNESS: every
// output is unconstrained (over-approximation), exactly as before.
(* mununu_missing_module *)
module keymgr_err
  import keymgr_pkg::*;
  import keymgr_reg_pkg::*;
(
  input clk_i,
  input rst_ni,
  input invalid_op_i,
  input disabled_i,
  input invalid_i,
  input kmac_input_invalid_i,
  input shadowed_update_err_i,
  input kmac_op_err_i,
  input invalid_kmac_out_i,
  input sideload_sel_err_i,
  input kmac_cmd_err_i,
  input kmac_fsm_err_i,
  input kmac_done_err_i,
  input regfile_intg_err_i,
  input shadowed_storage_err_i,
  input ctrl_fsm_err_i,
  input data_fsm_err_i,
  input op_fsm_err_i,
  input ecc_err_i,
  input state_change_err_i,
  input op_state_cmd_err_i,
  input cnt_err_i,
  input reseed_cnt_err_i,
  input sideload_fsm_err_i,
  input op_update_i,
  input op_done_i,
  output logic [SyncErrLastIdx-1:0] sync_err_o,
  output logic [AsyncErrLastIdx-1:0] async_err_o,
  output logic [SyncFaultLastIdx-1:0] sync_fault_o,
  output logic [AsyncFaultLastIdx-1:0] async_fault_o,
  output logic [ErrLastPos-1:0] error_o,
  output logic [FaultLastPos-1:0] fault_o
);
endmodule
