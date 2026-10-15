// The SVA forms the translator REJECTS today (and a few it accepts), one labelled assertion each,
// so their slang `--ast-json` shapes are on record rather than guessed. Captured 2026-10-14 in the
// `mununu-sva` image (slang 11.0.448+e222e7dc0):
//   slang --quiet --ast-json sva_fragment_forms.ast.json sva_fragment_forms.sv
// The truth-table test in `../translate.rs` (`the_shipped_fragment_truth_table`) spells the same
// shapes inline; this pair is the provenance. Not a verification fixture — nothing here lifts.
module t(input logic clk, rst_n, a, b, c, input logic [7:0] v, input logic [3:0] idx);
  logic [7:0] vec;
  always_ff @(posedge clk) vec <= v;
  p_sev:      assert property (@(posedge clk) disable iff (!rst_n) s_eventually b);
  p_sev_imp:  assert property (@(posedge clk) a |-> s_eventually b);
  p_sev_rng:  assert property (@(posedge clk) a |-> s_eventually [1:3] b);
  p_unb:      assert property (@(posedge clk) a |-> ##[1:$] b);
  p_strong:   assert property (@(posedge clk) a |-> strong(##[1:$] b));
  p_weak:     assert property (@(posedge clk) a |-> weak(##[1:$] b));
  p_unb_mid:  assert property (@(posedge clk) a |-> ##[1:$] b ##1 c);
  p_unbrep:   assert property (@(posedge clk) a |-> b[*1:$]);
  p_goto:     assert property (@(posedge clk) a |-> b[->2]);
  p_nonc:     assert property (@(posedge clk) a |-> b[=2]);
  p_thr:      assert property (@(posedge clk) a |-> (c throughout (b ##1 b)));
  p_cnt:      assert property (@(posedge clk) $countones(v) <= 2);
  p_unk:      assert property (@(posedge clk) !$isunknown(v));
  p_bit:      assert property (@(posedge clk) a |-> v[3]);
  p_rng_ante: assert property (@(posedge clk) (a ##[1:2] b) |-> c);
  p_evt:      assert property (@(posedge clk) a |-> eventually [1:3] b);
  p_nexttime: assert property (@(posedge clk) a |-> nexttime b);
  p_always:   assert property (@(posedge clk) always b);
endmodule
