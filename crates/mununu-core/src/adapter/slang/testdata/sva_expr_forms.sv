// OpenTitan-coverage Phase 2/3 forms: arithmetic, $past of an expression, concatenation and
// replication, a variable shift, dynamic selects, ?:, $onehot of a concatenation, signed
// comparisons and OpenTitan's single-return min / max helpers — one labelled assertion each.
// Captured 2026-10-15 in the mununu-sva image (slang 11.0.448):
//   slang --quiet --ast-json sva_expr_forms.ast.json sva_expr_forms.sv
// Consumed by `expression_forms_translate_from_real_slang_json` in ../translate.rs.
module t #(parameter int Width = 4, parameter int N = 4, parameter logic [Width-1:0] ResetValue = '0)
  (input logic clk, input logic [Width-1:0] step_i, input logic [N-1:0] req_i,
   input logic [$clog2(N)-1:0] idx_i, input logic signed [3:0] sx, input logic a,
   output logic [Width-1:0] cnt_o, output logic [1:0][Width-1:0] cnt_q, output logic [N-1:0] gnt_o);
  function automatic logic signed [Width+1:0] max(logic signed [Width+1:0] a, logic signed [Width+1:0] b);
    return (a > b) ? a : b;
  endfunction
  function automatic logic signed [Width+1:0] min(logic signed [Width+1:0] a, logic signed [Width+1:0] b);
    return (a < b) ? a : b;
  endfunction
  always_ff @(posedge clk) begin cnt_o <= cnt_o + step_i; cnt_q <= {cnt_q[0], cnt_o}; gnt_o <= req_i; end
  e_fwd:   assert property (@(posedge clk) a |=> cnt_q[1] == ({Width{1'b1}} - ResetValue));
  e_set:   assert property (@(posedge clk) a |=> cnt_q[1] == ({Width{1'b1}} - $past(step_i)));
  e_min:   assert property (@(posedge clk) a |=> cnt_o == min($past(cnt_o) + $past({2'b0, step_i}), {2'b0, {Width{1'b1}}}));
  e_max:   assert property (@(posedge clk) a |=> cnt_q[1] == max($past(signed'({2'b0, cnt_q[1]})) - $past({2'b0, step_i}), '0));
  e_sum:   assert property (@(posedge clk) ##1 $past((cnt_q[1] + cnt_q[0]) != {Width{1'b1}}) == a);
  e_rr:    assert property (@(posedge clk) ##1 |(req_i & ~((N'(1) << $past(idx_i)+1) - 1)) |-> idx_i > $past(idx_i));
  e_dyn:   assert property (@(posedge clk) a |-> gnt_o[idx_i] && req_i[idx_i]);
  e_oh:    assert property (@(posedge clk) $onehot({a, gnt_o[0], gnt_o[1]}));
  e_tern:  assert property (@(posedge clk) (a ? cnt_o : step_i) != 4'd9);
  e_sgn:   assert property (@(posedge clk) sx < 0 |-> sx >= -4'sd8);
  e_shr:   assert property (@(posedge clk) (cnt_o >> 2) <= 4'd3);
  e_cmp2:  assert property (@(posedge clk) (cnt_o + step_i) == (step_i + cnt_o));
endmodule
