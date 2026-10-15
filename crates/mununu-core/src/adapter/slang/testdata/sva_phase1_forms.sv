// OpenTitan-coverage Phase 1 forms: packed-struct members (incl. nested and under history calls),
// multi-dimensional packed elements, `->` / `<->`, 1-bit bitwise operators, constants folded by
// slang and a bare sequence property — one labelled assertion each.
// Captured 2026-10-15 in the mununu-sva image (slang 11.0.448):
//   slang --quiet --ast-json sva_phase1_forms.ast.json sva_phase1_forms.sv
// Consumed by `phase1_forms_translate_from_real_slang_json` in ../translate.rs.
package p;
  typedef struct packed { logic esc_p; logic esc_n; } esc_t;
  typedef struct packed { logic [3:0] hi; logic [1:0] mid; logic lo; } wide_t;
  typedef struct packed { esc_t tx; logic [2:0] cnt; } nest_t;
endpackage
module t import p::*; #(parameter int W = 4) (input logic clk, input esc_t e, input wide_t w,
    input nest_t n, input logic a, input logic b, output logic [1:0][W-1:0] cnt);
  always_ff @(posedge clk) cnt <= {cnt[0], cnt[1]};
  p_mem:     assert property (@(posedge clk) e.esc_p == e.esc_n |=> a);
  p_memw:    assert property (@(posedge clk) w.mid == 2'd1);
  p_nest:    assert property (@(posedge clk) n.tx.esc_n |-> n.cnt == 3'd5);
  p_xor:     assert property (@(posedge clk) e.esc_p ^ e.esc_n);
  p_rose:    assert property (@(posedge clk) $rose(e.esc_p) |-> a);
  p_pastm:   assert property (@(posedge clk) a |=> $past(w.hi) == 4'd3);
  p_md:      assert property (@(posedge clk) cnt[1] == 4'd3);
  p_mdst:    assert property (@(posedge clk) $stable(cnt[1]));
  p_band:    assert property (@(posedge clk) (a != b) & (a == 1'b1) |-> ~a | b);
  p_impl:    assert property (@(posedge clk) a -> ~b);
  p_equiv:   assert property (@(posedge clk) a <-> b);
  p_seq:     assert property (@(posedge clk) ##1 a == b);
  p_rep:     assert property (@(posedge clk) cnt[0] == {W{1'b1}});
  p_wide:    assert property (@(posedge clk) (cnt[0] & cnt[1]) == 4'd0);
endmodule
