// Stub of lowRISC OpenTitan `keymgr_data_en_state` standing in for a module this corpus entry never carried
// (port list from `hw/ip/keymgr/rtl/keymgr_data_en_state.sv` at 558921cc, Apache-2.0 — see UPSTREAM_COMMIT.txt). sv-elab refuses an
// instantiation with no definition (2026-11-04), where `read_verilog` lifted it with the cell
// undefined and its outputs anonymous free inputs. `(* mununu_missing_module *)` restores exactly
// that: the lift keeps the (empty, hence black-box) module only through the hierarchy snapshot —
// which names it in `diagnostics.blackboxed_modules` — and deletes it before the BTOR2 is written,
// so the cell is undefined again and its outputs are free, non-deliberate leaves: a VIOLATED over
// them is withheld as `violation-over-free-leaves` (mununu#750), as before. Without the attribute
// an empty module is a DELIBERATE `(* blackbox *)` cut that keeps its verdict. SOUNDNESS: every
// output is unconstrained (over-approximation), exactly as before.
(* mununu_missing_module *)
module keymgr_data_en_state
  import keymgr_pkg::*;
  import keymgr_reg_pkg::*;
(
  input clk_i,
  input rst_ni,
  input prim_mubi_pkg::mubi4_t hw_sel_i,
  input adv_en_i,
  input id_en_i,
  input gen_en_i,
  input op_done_i,
  input op_start_i,
  output logic data_hw_en_o,
  output logic data_sw_en_o,
  output logic fsm_err_o
);
endmodule
