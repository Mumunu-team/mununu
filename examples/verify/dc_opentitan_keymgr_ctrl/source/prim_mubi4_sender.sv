// Stub of lowRISC OpenTitan `prim_mubi4_sender` standing in for a module this corpus entry never carried
// (port list from `hw/ip/prim/rtl/prim_mubi4_sender.sv` at 558921cc, Apache-2.0 — see UPSTREAM_COMMIT.txt). sv-elab refuses an
// instantiation with no definition (2026-11-04), where `read_verilog` lifted it with the cell
// undefined and its outputs anonymous free inputs. `(* mununu_missing_module *)` restores exactly
// that: the lift keeps the (empty, hence black-box) module only through the hierarchy snapshot —
// which names it in `diagnostics.blackboxed_modules` — and deletes it before the BTOR2 is written,
// so the cell is undefined again and its outputs are free, non-deliberate leaves: a VIOLATED over
// them is withheld as `violation-over-free-leaves` (mununu#750), as before. Without the attribute
// an empty module is a DELIBERATE `(* blackbox *)` cut that keeps its verdict. SOUNDNESS: every
// output is unconstrained (over-approximation), exactly as before.
(* mununu_missing_module *)
module prim_mubi4_sender
  import prim_mubi_pkg::*;
#(
  parameter bit AsyncOn = 1,
  parameter bit EnSecBuf = 0,
  parameter mubi4_t ResetValue = MuBi4False
) (
  input          clk_i,
  input          rst_ni,
  input  mubi4_t mubi_i,
  output mubi4_t mubi_o
);
endmodule
