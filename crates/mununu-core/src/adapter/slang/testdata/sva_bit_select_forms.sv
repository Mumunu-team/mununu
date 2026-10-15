// Bit-selects and slices over non-zero-based, ascending, typedef'd, struct and multi-dimensional
// packed signals, history calls over a bit, and $isunknown — one labelled assertion each.
// Captured 2026-10-15 in the mununu-sva image (slang 11.0.448):
//   slang --quiet --ast-json sva_bit_select_forms.ast.json sva_bit_select_forms.sv
// Consumed by `bit_selects_and_slices_map_sv_indices_to_yosys_bit_positions` in ../translate.rs.
module t(input logic clk, input logic [15:8] off, input logic [0:7] asc, input logic [7:0] v);
  typedef logic [7:0] byte_t;
  typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pair_t;
  byte_t tb; pair_t ps; logic [3:0][7:0] md;
  always_ff @(posedge clk) begin tb <= v; ps <= v; md <= {v,v,v,v}; end
  p_off_bit:  assert property (@(posedge clk) off[9]);
  p_off_rng:  assert property (@(posedge clk) off[11:8] == 4'd3);
  p_asc_bit:  assert property (@(posedge clk) asc[1]);
  p_asc_rng:  assert property (@(posedge clk) asc[0:3] == 4'd3);
  p_td_bit:   assert property (@(posedge clk) tb[2]);
  p_st_bit:   assert property (@(posedge clk) ps[5]);
  p_md_el:    assert property (@(posedge clk) md[1] == 8'd2);
  p_rose:     assert property (@(posedge clk) $rose(v[3]));
  p_stable:   assert property (@(posedge clk) $stable(v[7:4]));
  p_past:     assert property (@(posedge clk) v[2] == $past(v[2]));
  p_unk:      assert property (@(posedge clk) !$isunknown(v));
endmodule
