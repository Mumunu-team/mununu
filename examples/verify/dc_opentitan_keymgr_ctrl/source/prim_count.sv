// Stub of lowRISC OpenTitan `prim_count` standing in for a module this corpus entry never carried
// (port list from `examples/verify/dc_opentitan_prim_count/source/prim_count.sv` at 558921cc, Apache-2.0 — see UPSTREAM_COMMIT.txt). sv-elab refuses an
// instantiation with no definition (2026-11-04), where `read_verilog` lifted it with the cell
// undefined and its outputs anonymous free inputs. `(* mununu_missing_module *)` restores exactly
// that: the lift keeps the (empty, hence black-box) module only through the hierarchy snapshot —
// which names it in `diagnostics.blackboxed_modules` — and deletes it before the BTOR2 is written,
// so the cell is undefined again and its outputs are free, non-deliberate leaves: a VIOLATED over
// them is withheld as `violation-over-free-leaves` (mununu#750), as before. Without the attribute
// an empty module is a DELIBERATE `(* blackbox *)` cut that keeps its verdict. SOUNDNESS: every
// output is unconstrained (over-approximation), exactly as before.
(* mununu_missing_module *)
module prim_count #(
  parameter int               Width = 2,
  parameter logic [Width-1:0] ResetValue = '0,
  parameter bit               EnableAlertTriggerSVA = 1
) (
  input clk_i,
  input rst_ni,
  input clr_i,
  input set_i,
  input [Width-1:0] set_cnt_i,
  input incr_en_i,
  input decr_en_i,
  input [Width-1:0] step_i,
  input commit_i,
  output logic [Width-1:0] cnt_o,
  output logic [Width-1:0] cnt_after_commit_o,
  output logic err_o
);
endmodule
