// Stub of lowRISC OpenTitan `keymgr_op_state_ctrl` standing in for a module this corpus entry never carried
// (port list from `hw/ip/keymgr/rtl/keymgr_op_state_ctrl.sv` at 558921cc, Apache-2.0 — see UPSTREAM_COMMIT.txt). sv-elab refuses an
// instantiation with no definition (2026-11-04), where `read_verilog` lifted it with the cell
// undefined and its outputs anonymous free inputs. `(* mununu_missing_module *)` restores exactly
// that: the lift keeps the (empty, hence black-box) module only through the hierarchy snapshot —
// which names it in `diagnostics.blackboxed_modules` — and deletes it before the BTOR2 is written,
// so the cell is undefined again and its outputs are free, non-deliberate leaves: a VIOLATED over
// them is withheld as `violation-over-free-leaves` (mununu#750), as before. Without the attribute
// an empty module is a DELIBERATE `(* blackbox *)` cut that keeps its verdict. SOUNDNESS: every
// output is unconstrained (over-approximation), exactly as before.
(* mununu_missing_module *)
module keymgr_op_state_ctrl
  import keymgr_pkg::*;
  import keymgr_reg_pkg::*;
(
  input clk_i,
  input rst_ni,
  input adv_req_i,
  input dis_req_i,
  input id_req_i,
  input gen_req_i,
  input [CdiWidth-1:0] cnt_i,
  output logic op_ack_o,
  output logic op_busy_o,
  output logic op_update_o,
  input kmac_done_i,
  output logic adv_en_o,
  output logic id_en_o,
  output logic gen_en_o,
  output logic op_fsm_err_o
);
endmodule
