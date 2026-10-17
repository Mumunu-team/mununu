// Assertions inside sub-instances and hierarchical references (the OpenTitan `prim_esc_rxtx`
// harness's shapes): a body two instances share (slang serialises it once and refers to it by
// address), a parameter overridden per instance whose NAME the top also declares, a top-level
// signal with the same name as a sub-instance's register, `if` generates (one branch not
// elaborated), a `for` generate, and hierarchical references from the top.
// Captured 2026-10-16 in the mununu-sva image (slang 11.0.448):
//   slang --quiet --ast-json sva_hierarchy_forms.ast.json sva_hierarchy_forms.sv
// Consumed by `hierarchy_forms_translate_from_real_slang_json` in ../translate.rs.
module leaf #(parameter int W = 2) (input logic clk, input logic [W-1:0] d);
  logic [W-1:0] q_r;
  always_ff @(posedge clk) q_r <= d;
  a_leaf: assert property (@(posedge clk) q_r != W);
  if (W == 2) begin : g_two
    logic t;
    assign t = q_r[0];
    a_gen: assert property (@(posedge clk) t == q_r[0]);
  end else begin : g_other
    a_other: assert property (@(posedge clk) q_r != 0);
  end
endmodule

module top (input logic clk, input logic [1:0] d, output logic [1:0] q_r);
  parameter int W = 7;
  assign q_r = 2'd0;
  leaf #(.W(2)) u_a (.clk, .d);
  leaf #(.W(2)) u_b (.clk, .d(~d));
  leaf #(.W(3)) u_d (.clk, .d({d, 1'b0}));
  for (genvar i = 0; i < 2; i++) begin : g_arr
    leaf #(.W(2)) u_c (.clk, .d);
  end
  a_hier_unique: assert property (@(posedge clk) u_d.q_r != 3'd7);
  a_hier_shared: assert property (@(posedge clk) u_a.q_r != 2'd3);
  a_top: assert property (@(posedge clk) q_r == 2'd0);
endmodule
